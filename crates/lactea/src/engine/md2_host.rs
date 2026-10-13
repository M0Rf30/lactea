//! Host-facing BeatDrop inputs of [`Engine`]: the clock, the arrow keys and the `monitor` variable
//! (`CompatRules::md2_input_vars`, see [`crate::md2_inputs`]).

use super::Engine;
use crate::md2_inputs::{CtrlKeys, DateTime};

impl Engine {
    /// MilkDrop 2 (BeatDrop): the wall clock behind `year month day weekday hour minute second milliseconds
    /// totalseconds` and the shader constants `_c15` / `_c16`. The engine never reads the system clock; call this
    /// whenever the host's time moves on (typically once per rendered frame). Fields are used as given.
    ///
    /// Until the first call (and after [`Engine::clear_date_time`]) the date reads 0 and the time of day follows
    /// the frame clock: `totalseconds` is the frame time modulo 86400 seconds, `hour` / `minute` / `second` /
    /// `milliseconds` its parts. Ignored outside `Compat::MilkDrop2`.
    pub fn set_date_time(&mut self, date_time: DateTime) {
        self.md2_in.set_date_time(Some(date_time));
    }

    /// Go back to the frame-clock time of day of [`Engine::set_date_time`].
    pub fn clear_date_time(&mut self) {
        self.md2_in.set_date_time(None);
    }

    /// MilkDrop 2 (BeatDrop): the Ctrl + arrow keys behind `ctrllt ctrlrt ctrldn ctrlup`; the engine samples the
    /// latest state once per rendered frame. Ignored outside `Compat::MilkDrop2`.
    pub fn set_ctrl_keys(&mut self, keys: CtrlKeys) {
        self.md2_in.set_ctrl_keys(keys);
    }

    /// The preset's `monitor` variable as the last rendered frame of the active preset left it (0 when no preset
    /// is loaded or its code never mentions `monitor`). BeatDrop registers it in the per-frame context only
    /// (`state.cpp:328`; `plugin.cpp:353-356`: readable and writable in init and per-frame code, not from per-pixel
    /// code). It persists from frame to frame: `milkdropfs.cpp:616` reloads `monitor_after_init_code` at the start of
    /// every frame and `:838` stores the per-frame result back after the code ran. A preset load starts it from the
    /// init code's value (`state.cpp:1682-1684, 1800`), 0 when the init code never writes it. Here it is a variable of
    /// the per-frame context that nothing resets between frames, in every compat mode.
    pub fn monitor(&self) -> f64 {
        self.active.as_ref().map_or(0.0, |slot| {
            let pf = &slot.loaded.pf;
            pf.ctx.find_var("monitor").map_or(0.0, |v| pf.ctx.get(v))
        })
    }
}
