//! Preset-switch control: projectM's host-facing settings and the `TimeKeeper` /
//! `ProjectM::RenderFrame` logic that decides **when** a preset change is wanted.
//!
//! The engine itself never switches presets: [`Engine::load_preset`](crate::Engine::load_preset) is
//! always an explicit host call. [`PresetControl`] is the part of projectM that watches the clock and
//! the loudness and tells the host "switch now" (`projectm_preset_switch_requested_event`), with
//! projectM's settings, defaults and quirks:
//!
//! | projectM API | here | default |
//! |---|---|---|
//! | `projectm_set_preset_duration` | [`PresetControl::set_preset_duration`] | 30 s |
//! | `projectm_set_soft_cut_duration` | [`PresetControl::set_soft_cut_duration`] | 3 s |
//! | `projectm_set_hard_cut_{enabled,duration,sensitivity}` | `set_hard_cut_*` | off, 20 s, 2.0 |
//! | `projectm_set_easter_egg` | [`PresetControl::set_easter_egg`] | 1.0 |
//! | `projectm_set_preset_locked` | [`PresetControl::set_preset_locked`] | unlocked |
//! | `projectm_set_beat_sensitivity` | [`PresetControl::set_beat_sensitivity`] (clamped 0..2) | 1.0 |
//! | `projectm_set_aspect_correction` | [`PresetControl::set_aspect_correction`] | on |
//!
//! `beat_sensitivity` and `aspect_correction` are stored and returned, exactly like projectM, which
//! never reads them anywhere else (`ProjectM.cpp`: only the setters/getters reference the fields).
//!
//! Usage: call [`PresetControl::start_preset`] (hard cut) or [`PresetControl::start_smoothing`]
//! (soft cut) whenever the host loads a preset, and [`PresetControl::update`] once per rendered frame
//! with the frame time and the frame's `vol` ([`AudioFrame::vol`](lactea_audio::AudioFrame::vol));
//! it returns `Some(SwitchRequest)` once per preset when the host should switch. Right after either start call,
//! hand [`PresetControl::preset_duration_sampled`] to
//! [`Engine::set_loaded_preset_duration`](crate::Engine::set_loaded_preset_duration) so the preset's `progress` runs
//! over the same display time (the C API and `lactea-app` do this at their single load funnel).
//!
//! This module only knows about time and loudness: nothing here depends on blending.

use crate::callbacks::Hook;
use crate::rng::StreamRng;

/// A hard cut needs more than this many rendered frames first (`m_frameCount > 50`).
pub const HARD_CUT_MIN_FRAMES: u64 = 50;

/// The host should load another preset (`projectm_preset_switch_requested_event`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwitchRequest {
    /// `true`: beat-driven hard cut (instant switch); `false`: the display time is up (soft cut).
    pub hard_cut: bool,
}

/// projectM's timing + switch-request state. Deterministic: the easter-egg duration sampling is
/// seeded from the run seed.
#[derive(Clone, Debug)]
pub struct PresetControl {
    beat_sensitivity: f32,
    aspect_correction: bool,
    easter_egg: f32,
    locked: bool,
    preset_duration: f64,
    soft_cut_duration: f64,
    /// What `TimeKeeper` uses (the exact value passed to the setter).
    hard_cut_duration: f64,
    /// What `projectm_get_hard_cut_duration` returns (projectM stores `static_cast<int>(seconds)`).
    hard_cut_duration_reported: f64,
    hard_cut_enabled: bool,
    hard_cut_sensitivity: f32,

    now: f64,
    preset_time: f64,
    preset_duration_a: f64,
    preset_duration_b: f64,
    smoothing: bool,
    smooth_start: f64,
    notified: bool,
    frame_count: u64,
    previous_vol: f32,
    rng: StreamRng,
    on_request: Hook<SwitchRequest>,
    /// MD3 `TimeBetweenPresetsRand` while the MD3 display-time rule is on (`None`: projectM's gaussian).
    md3_time_rand: Option<f64>,
}

impl PresetControl {
    /// projectM's defaults. `seed` drives the easter-egg duration sampling.
    pub fn new(seed: u64) -> Self {
        let mut c = Self {
            beat_sensitivity: 1.0,
            aspect_correction: true,
            easter_egg: 1.0,
            locked: false,
            preset_duration: 30.0,
            soft_cut_duration: 3.0,
            hard_cut_duration: 20.0,
            hard_cut_duration_reported: 20.0,
            hard_cut_enabled: false,
            hard_cut_sensitivity: 2.0,
            now: 0.0,
            preset_time: 0.0,
            preset_duration_a: 0.0,
            preset_duration_b: 0.0,
            smoothing: false,
            smooth_start: 0.0,
            notified: false,
            frame_count: 0,
            previous_vol: 0.0,
            rng: StreamRng::new(seed, "preset_duration"),
            on_request: Hook::none(),
            md3_time_rand: None,
        };
        c.preset_duration_a = c.sampled_preset_duration();
        c
    }

    /// `projectm_set_preset_switch_requested_event_callback`: called from [`PresetControl::update`]
    /// with every request it returns (`None` removes the callback).
    pub fn set_switch_requested_callback(
        &mut self,
        f: Option<impl Fn(&SwitchRequest) + Send + Sync + 'static>,
    ) {
        self.on_request.set(f);
    }

    /// MilkDrop 3 display time (`CompatRules::md3_preset_time_rand`, `docs/md3.md`): with `Some(rand)` the
    /// display time of a preset is `soft cut duration (BlendTimeAuto) + preset duration (TimeBetweenPresets) +
    /// (r % 1000) * rand * 0.001` with `r` drawn from the seeded stream, instead of projectM's gaussian
    /// (the easter egg is then ignored). `rand` is `TimeBetweenPresetsRand` (MD3 default 10 s); negative
    /// values count as 0. Applies to presets started afterwards.
    pub fn set_md3_time_rand(&mut self, rand: Option<f64>) {
        self.md3_time_rand = rand.map(|r| r.max(0.0));
    }
    /// The MD3 display-time randomisation set by [`set_md3_time_rand`](Self::set_md3_time_rand).
    pub fn md3_time_rand(&self) -> Option<f64> {
        self.md3_time_rand
    }

    // ---- settings -------------------------------------------------------------------------

    /// `projectm_set_beat_sensitivity`: clamped to `0..=2`.
    pub fn set_beat_sensitivity(&mut self, sensitivity: f32) {
        self.beat_sensitivity = sensitivity.clamp(0.0, 2.0);
    }
    /// Current beat sensitivity (`projectm_get_beat_sensitivity`).
    pub fn beat_sensitivity(&self) -> f32 {
        self.beat_sensitivity
    }

    /// Whether aspect correction is enabled for presets.
    pub fn set_aspect_correction(&mut self, enabled: bool) {
        self.aspect_correction = enabled;
    }
    /// Whether aspect correction is enabled.
    pub fn aspect_correction(&self) -> bool {
        self.aspect_correction
    }

    /// `projectm_set_easter_egg`: the sigma of the gaussian preset display time (see
    /// [`PresetControl::sampled_preset_duration`]); below 0.001 the duration is exact.
    pub fn set_easter_egg(&mut self, value: f32) {
        self.easter_egg = value;
    }
    /// Current easter-egg sigma.
    pub fn easter_egg(&self) -> f32 {
        self.easter_egg
    }

    /// `projectm_set_preset_locked`: a locked control never emits a request. Locking marks the
    /// current preset as notified; **unlocking clears that mark** (projectM has a ToDo for a timer
    /// reset), so a preset whose display time already ran out requests a switch on the next frame.
    pub fn set_preset_locked(&mut self, locked: bool) {
        self.locked = locked;
        self.notified = locked;
    }
    /// Whether the control is locked on the current preset.
    pub fn preset_locked(&self) -> bool {
        self.locked
    }

    /// Display time of a preset in seconds; applies to presets started afterwards.
    pub fn set_preset_duration(&mut self, seconds: f64) {
        self.preset_duration = seconds;
    }
    /// Display time of a preset in seconds.
    pub fn preset_duration(&self) -> f64 {
        self.preset_duration
    }

    /// Length of a soft cut; the smoothing state ends this long after [`start_smoothing`](Self::start_smoothing).
    pub fn set_soft_cut_duration(&mut self, seconds: f64) {
        self.soft_cut_duration = seconds;
    }
    /// Length of a soft cut in seconds.
    pub fn soft_cut_duration(&self) -> f64 {
        self.soft_cut_duration
    }

    /// Enable or disable hard cuts.
    pub fn set_hard_cut_enabled(&mut self, enabled: bool) {
        self.hard_cut_enabled = enabled;
    }
    /// Whether hard cuts are enabled.
    pub fn hard_cut_enabled(&self) -> bool {
        self.hard_cut_enabled
    }

    /// Minimum display time before a hard cut. projectM's *getter* returns the value truncated to
    /// an integer (`m_hardCutDuration = static_cast<int>(seconds)`) while the timer uses the exact
    /// value; both are reproduced.
    pub fn set_hard_cut_duration(&mut self, seconds: f64) {
        self.hard_cut_duration = seconds;
        self.hard_cut_duration_reported = seconds.trunc();
    }
    /// Minimum hard-cut display time, truncated to whole seconds like projectM's getter.
    // The getter deliberately reports the truncated value (projectM's behaviour), not the exact one.
    #[allow(clippy::misnamed_getters)]
    pub fn hard_cut_duration(&self) -> f64 {
        self.hard_cut_duration_reported
    }

    /// Volume jump (`vol - previous vol`) that triggers a hard cut.
    pub fn set_hard_cut_sensitivity(&mut self, sensitivity: f32) {
        self.hard_cut_sensitivity = sensitivity;
    }
    /// Volume jump that triggers a hard cut.
    pub fn hard_cut_sensitivity(&self) -> f32 {
        self.hard_cut_sensitivity
    }

    // ---- timing ---------------------------------------------------------------------------

    /// `TimeKeeper::sampledPresetDuration`: the preset duration, or (easter egg ≥ 0.001) a gaussian
    /// sample with that mean and the easter egg as sigma, at least 1 s. With
    /// [`set_md3_time_rand`](Self::set_md3_time_rand) the MD3 formula replaces both.
    pub fn sampled_preset_duration(&mut self) -> f64 {
        if let Some(rand) = self.md3_time_rand {
            // `(rand() % 1000) * TimeBetweenPresetsRand * 0.001`: a 0.001 s grid below `rand`
            let r = f64::from(self.rng.next32() % 1000);
            return self.soft_cut_duration + self.preset_duration + r * rand * 0.001;
        }
        if f64::from(self.easter_egg) < 0.001 {
            return self.preset_duration;
        }
        // Box-Muller from the seeded stream (u1 in (0, 1], u2 in [0, 1)).
        let u1 = ((self.rng.next64() >> 11) as f64 + 1.0) / (1u64 << 53) as f64;
        let u2 = (self.rng.next64() >> 11) as f64 / (1u64 << 53) as f64;
        let z = (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos();
        (self.preset_duration + f64::from(self.easter_egg) * z).max(1.0)
    }

    /// A preset was loaded with a hard cut at `time` (`TimeKeeper::StartPreset`): restarts its
    /// display time and re-arms the switch request (`m_presetChangeNotified = m_presetLocked`).
    pub fn start_preset(&mut self, time: f64) {
        self.now = time;
        self.notified = self.locked;
        self.smoothing = false;
        self.preset_time = time;
        self.preset_duration_a = self.sampled_preset_duration();
    }

    /// A preset was loaded with a soft cut at `time` (`TimeKeeper::StartSmoothing`). The new
    /// preset's timer starts when the soft cut ends. A soft cut during a soft cut finishes the
    /// first one immediately, like projectM.
    pub fn start_smoothing(&mut self, time: f64) {
        self.now = time;
        self.notified = self.locked;
        if self.smoothing {
            self.end_smoothing();
        }
        self.smoothing = true;
        self.smooth_start = time;
        self.preset_duration_b = self.sampled_preset_duration();
    }

    /// `TimeKeeper::EndSmoothing`: the incoming preset becomes the displayed one, with the duration sampled when its
    /// soft cut started.
    fn end_smoothing(&mut self) {
        self.smoothing = false;
        self.preset_time = self.smooth_start;
        self.preset_duration_a = self.preset_duration_b;
    }

    /// The display time `TimeKeeper` sampled for the preset started last (the incoming one during a soft cut):
    /// what its `progress` runs over. Pass it to
    /// [`Engine::set_loaded_preset_duration`](crate::Engine::set_loaded_preset_duration) right after loading.
    pub fn preset_duration_sampled(&self) -> f64 {
        if self.smoothing {
            self.preset_duration_b
        } else {
            self.preset_duration_a
        }
    }

    /// Whether a soft cut is currently running.
    pub fn is_smoothing(&self) -> bool {
        self.smoothing
    }

    /// Time of the last [`update`](Self::update) / `start_*` (`projectm_get_last_frame_time`).
    pub fn frame_time(&self) -> f64 {
        self.now
    }

    /// `TimeKeeper::PresetProgressA`: 0..1 over the displayed preset's (sampled) duration; 1 while
    /// a soft cut is running.
    pub fn preset_progress(&self) -> f64 {
        if self.smoothing {
            return 1.0;
        }
        ((self.now - self.preset_time) / self.preset_duration_a).min(1.0)
    }

    /// `TimeKeeper::CanHardCut`.
    fn can_hard_cut(&self) -> bool {
        self.now - self.preset_time > self.hard_cut_duration
    }

    /// One rendered frame at `time` seconds whose audio analysis gave `vol`
    /// (`AudioFrame::vol`). Returns the switch request, at most one per started preset:
    ///
    /// * display time up (`progress >= 1` and no soft cut running): `hard_cut = false`;
    /// * else, with hard cuts enabled, more than [`HARD_CUT_MIN_FRAMES`] frames rendered, `vol`
    ///   having risen by more than the hard-cut sensitivity since the previous frame and the preset
    ///   shown for longer than the hard-cut duration: `hard_cut = true`.
    pub fn update(&mut self, time: f64, vol: f32) -> Option<SwitchRequest> {
        self.update_with_auto_cuts(time, vol, 0)
    }

    /// [`update`](Self::update) plus the instant preset changes MD3's auto hard cut asked for this
    /// frame (`Engine::take_auto_cuts`, `docs/md3.md`). `auto_cuts > 0` returns a `hard_cut = true`
    /// request even when the display time is also up (the cut wins; one request per call however many
    /// cuts, the host loads one preset), independent of the projectM hard-cut settings and of the
    /// preset's age. A locked control never emits a request, so a locked preset ignores auto cuts.
    pub fn update_with_auto_cuts(
        &mut self,
        time: f64,
        vol: f32,
        auto_cuts: u32,
    ) -> Option<SwitchRequest> {
        self.now = time;
        let mut request = None;
        if !self.notified {
            if self.preset_progress() >= 1.0 && !self.smoothing {
                self.notified = true;
                request = Some(SwitchRequest { hard_cut: false });
            } else if self.hard_cut_enabled
                && self.frame_count > HARD_CUT_MIN_FRAMES
                && vol - self.previous_vol > self.hard_cut_sensitivity
                && self.can_hard_cut()
            {
                self.notified = true;
                request = Some(SwitchRequest { hard_cut: true });
            }
        }
        if auto_cuts > 0 && !self.locked {
            self.notified = true;
            request = Some(SwitchRequest { hard_cut: true });
        }
        if self.smoothing && (self.now - self.smooth_start) / self.soft_cut_duration >= 1.0 {
            self.end_smoothing();
        }
        self.frame_count += 1;
        self.previous_vol = vol;
        if let Some(r) = &request {
            self.on_request.fire(r);
        }
        request
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(
        c: &mut PresetControl,
        from: u32,
        to: u32,
        fps: f64,
        vol: impl Fn(u32) -> f32,
    ) -> Vec<(u32, SwitchRequest)> {
        let mut out = Vec::new();
        for n in from..to {
            if let Some(r) = c.update(f64::from(n) / fps, vol(n)) {
                out.push((n, r));
            }
        }
        out
    }

    #[test]
    fn defaults_and_clamps_follow_projectm() {
        let mut c = PresetControl::new(1);
        assert_eq!(c.beat_sensitivity(), 1.0);
        assert!(c.aspect_correction());
        assert_eq!(c.easter_egg(), 1.0);
        assert!(!c.preset_locked() && !c.hard_cut_enabled());
        assert_eq!((c.preset_duration(), c.soft_cut_duration()), (30.0, 3.0));
        assert_eq!(
            (c.hard_cut_duration(), c.hard_cut_sensitivity()),
            (20.0, 2.0)
        );
        c.set_beat_sensitivity(7.0);
        assert_eq!(c.beat_sensitivity(), 2.0);
        c.set_beat_sensitivity(-1.0);
        assert_eq!(c.beat_sensitivity(), 0.0);
        c.set_beat_sensitivity(0.25);
        assert_eq!(c.beat_sensitivity(), 0.25);
        c.set_aspect_correction(false);
        assert!(!c.aspect_correction());
        // the getter returns the truncated integer, the timer keeps the exact value
        c.set_hard_cut_duration(12.75);
        assert_eq!(c.hard_cut_duration(), 12.0);
    }

    #[test]
    fn display_time_up_requests_a_soft_cut_once() {
        let mut c = PresetControl::new(1);
        c.set_easter_egg(0.0); // exact duration
        c.set_preset_duration(2.0);
        c.start_preset(0.0);
        let got = run(&mut c, 0, 600, 60.0, |_| 0.0);
        // progress reaches 1 at t = 2 s = frame 120; only one request until the next start_preset
        assert_eq!(got, vec![(120, SwitchRequest { hard_cut: false })]);
        c.start_preset(10.0);
        assert_eq!(c.preset_progress(), 0.0);
        assert!(run(&mut c, 600, 700, 60.0, |_| 0.0).is_empty()); // 10 s .. 11.6 s
        assert_eq!(run(&mut c, 700, 800, 60.0, |_| 0.0).len(), 1);
    }

    #[test]
    fn hard_cut_needs_flag_frames_minimum_time_and_a_volume_jump() {
        let spike = |n: u32| if n == 900 { 5.0 } else { 0.0 };
        let mut c = PresetControl::new(1);
        c.set_easter_egg(0.0);
        c.set_preset_duration(1e6);
        c.set_hard_cut_duration(10.0);
        c.start_preset(0.0);
        // disabled: nothing
        assert!(run(&mut c, 0, 1000, 60.0, spike).is_empty());

        let mut c = PresetControl::new(1);
        c.set_easter_egg(0.0);
        c.set_preset_duration(1e6);
        c.set_hard_cut_enabled(true);
        c.set_hard_cut_duration(10.0);
        c.start_preset(0.0);
        // a spike at frame 30 (0.5 s) is too early (min time and > 50 frames), the one at 900 (15 s) fires
        let vol = |n: u32| if n == 30 || n == 900 { 5.0 } else { 0.0 };
        assert_eq!(
            run(&mut c, 0, 1000, 60.0, vol),
            vec![(900, SwitchRequest { hard_cut: true })]
        );
        // sensitivity: a jump of 1.5 does not pass 2.0 but passes 1.0
        let mut c = PresetControl::new(1);
        c.set_easter_egg(0.0);
        c.set_preset_duration(1e6);
        c.set_hard_cut_enabled(true);
        c.set_hard_cut_duration(1.0);
        c.start_preset(0.0);
        let jump = |n: u32| if n == 300 { 1.5 } else { 0.0 };
        assert!(run(&mut c, 0, 400, 60.0, jump).is_empty());
        let mut c = PresetControl::new(1);
        c.set_easter_egg(0.0);
        c.set_preset_duration(1e6);
        c.set_hard_cut_enabled(true);
        c.set_hard_cut_duration(1.0);
        c.set_hard_cut_sensitivity(1.0);
        c.start_preset(0.0);
        assert_eq!(run(&mut c, 0, 400, 60.0, jump).len(), 1);
    }

    #[test]
    fn lock_suppresses_requests_and_unlock_rearms_the_current_preset() {
        let mut c = PresetControl::new(1);
        c.set_easter_egg(0.0);
        c.set_preset_duration(1.0);
        c.set_preset_locked(true);
        c.start_preset(0.0);
        assert!(run(&mut c, 0, 300, 60.0, |_| 0.0).is_empty());
        // projectM: unlocking clears the "notified" mark; the display time is long over -> fires at once
        c.set_preset_locked(false);
        assert_eq!(
            run(&mut c, 300, 301, 60.0, |_| 0.0),
            vec![(300, SwitchRequest { hard_cut: false })]
        );
        assert!(run(&mut c, 301, 600, 60.0, |_| 0.0).is_empty());
        c.start_preset(10.0);
        assert_eq!(run(&mut c, 600, 800, 60.0, |_| 0.0).len(), 1);
    }

    #[test]
    fn soft_cut_state_machine_blocks_requests_then_restarts_the_timer() {
        let mut c = PresetControl::new(1);
        c.set_easter_egg(0.0);
        c.set_preset_duration(5.0);
        c.set_soft_cut_duration(2.0);
        c.start_preset(0.0);
        c.start_smoothing(1.0);
        assert!(c.is_smoothing());
        assert_eq!(c.preset_progress(), 1.0); // progress A is pinned to 1 while smoothing
        // t = 1 .. 3: smoothing, no request although progress is 1; ends at t = 3
        assert!(run(&mut c, 60, 179, 60.0, |_| 0.0).is_empty());
        assert!(c.is_smoothing());
        run(&mut c, 179, 182, 60.0, |_| 0.0);
        assert!(!c.is_smoothing());
        // the incoming preset's timer started with the soft cut (t = 1): due at t = 6
        let got = run(&mut c, 182, 400, 60.0, |_| 0.0);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, 360);
        assert!(!got[0].1.hard_cut);
    }

    #[test]
    fn soft_cut_preset_keeps_the_duration_sampled_when_its_cut_started() {
        let mut c = PresetControl::new(5);
        c.set_easter_egg(5.0);
        c.set_preset_duration(30.0);
        c.set_soft_cut_duration(2.0);
        c.start_preset(0.0);
        let shown = c.preset_duration_sampled();
        c.start_smoothing(10.0);
        let incoming = c.preset_duration_sampled();
        assert_ne!(
            shown, incoming,
            "the incoming preset has a sample of its own"
        );
        run(&mut c, 600, 750, 60.0, |_| 0.0); // the soft cut ends at t = 12
        assert!(!c.is_smoothing());
        assert_eq!(c.preset_duration_sampled(), incoming);
        // 15 s into the incoming preset (it started with its soft cut at t = 10)
        c.update(25.0, 0.0);
        assert!((c.preset_progress() - 15.0 / incoming).abs() < 1e-12);
    }

    #[test]
    fn easter_egg_samples_a_seeded_gaussian_of_at_least_one_second() {
        let mut a = PresetControl::new(7);
        let mut b = PresetControl::new(7);
        a.set_easter_egg(5.0);
        b.set_easter_egg(5.0);
        let xs: Vec<f64> = (0..2000).map(|_| a.sampled_preset_duration()).collect();
        let ys: Vec<f64> = (0..2000).map(|_| b.sampled_preset_duration()).collect();
        assert_eq!(xs, ys, "same seed, same samples");
        assert!(xs.iter().all(|&x| x >= 1.0));
        let mean = xs.iter().sum::<f64>() / xs.len() as f64;
        let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / xs.len() as f64;
        assert!((mean - 30.0).abs() < 0.5, "mean {mean}");
        assert!((var.sqrt() - 5.0).abs() < 0.5, "sigma {}", var.sqrt());
        assert!(xs.iter().any(|&x| (x - 30.0).abs() > 1.0), "durations vary");
        a.set_easter_egg(0.0005);
        assert_eq!(a.sampled_preset_duration(), 30.0);
        let mut other = PresetControl::new(8);
        other.set_easter_egg(5.0);
        assert_ne!(other.sampled_preset_duration(), xs[0]);
    }

    #[test]
    fn auto_cuts_request_a_hard_cut_independent_of_the_projectm_settings() {
        let mut c = PresetControl::new(1);
        c.set_easter_egg(0.0);
        c.set_preset_duration(1e6);
        c.start_preset(0.0);
        // hard cuts are off and the preset is brand new: a cut is requested anyway
        let r = c.update_with_auto_cuts(0.5, 0.0, 1);
        assert_eq!(r, Some(SwitchRequest { hard_cut: true }));
        // two cuts in one frame (mode 4) are still one request
        c.start_preset(1.0);
        assert_eq!(
            c.update_with_auto_cuts(1.1, 0.0, 2),
            Some(SwitchRequest { hard_cut: true })
        );
        // no cuts, no request
        c.start_preset(2.0);
        assert_eq!(c.update_with_auto_cuts(2.1, 0.0, 0), None);
    }

    #[test]
    fn an_auto_cut_wins_over_the_display_time_and_a_lock_suppresses_it() {
        let mut c = PresetControl::new(1);
        c.set_easter_egg(0.0);
        c.set_preset_duration(1.0);
        c.start_preset(0.0);
        assert_eq!(
            c.update_with_auto_cuts(2.0, 0.0, 1),
            Some(SwitchRequest { hard_cut: true })
        );
        c.start_preset(3.0);
        c.set_preset_locked(true);
        assert_eq!(c.update_with_auto_cuts(3.5, 0.0, 1), None);
    }

    #[test]
    fn md3_display_time_adds_the_blend_time_and_a_seeded_random_part() {
        let mut c = PresetControl::new(3);
        c.set_preset_duration(16.0);
        c.set_soft_cut_duration(2.7);
        c.set_md3_time_rand(Some(10.0));
        assert_eq!(c.md3_time_rand(), Some(10.0));
        let xs: Vec<f64> = (0..500).map(|_| c.sampled_preset_duration()).collect();
        // 2.7 + 16 + (0..=999) * 10 * 0.001 -> [18.7, 28.69]
        assert!(xs.iter().all(|&x| (18.7..=28.69 + 1e-9).contains(&x)));
        assert!(xs.iter().any(|&x| x > 25.0) && xs.iter().any(|&x| x < 22.0));
        let mut d = PresetControl::new(3);
        d.set_preset_duration(16.0);
        d.set_soft_cut_duration(2.7);
        d.set_md3_time_rand(Some(10.0));
        let ys: Vec<f64> = (0..500).map(|_| d.sampled_preset_duration()).collect();
        assert_eq!(xs, ys, "same seed, same samples");
        // Rand = 0 is the exact window; the easter egg is ignored; a negative Rand counts as 0
        c.set_md3_time_rand(Some(-4.0));
        assert_eq!(c.md3_time_rand(), Some(0.0));
        c.set_easter_egg(5.0);
        assert!((c.sampled_preset_duration() - 18.7).abs() < 1e-12);
        // off again: projectM's gaussian
        c.set_md3_time_rand(None);
        c.set_easter_egg(0.0);
        assert_eq!(c.sampled_preset_duration(), 16.0);
    }
}
