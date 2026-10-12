//! MilkDrop 3 inputs of the expressions and shaders that come from the host rather than from the preset: the mouse,
//! the shader FFT (`sampler_fft`) and the sticky `FFTAttack` / `FFTDecay` globals (`docs/md3.md`, shader inputs).
//!
//! Everything is sampled **once per rendered frame** at the top of [`crate::Engine::render`], like MD3 does at the
//! top of its render function, and shared by every state drawn that frame (the incoming and the outgoing preset of a
//! soft cut see the same cursor and the same spectrum).

use crate::EngineConfig;
use crate::runtime::PerFrame;
use lactea_abi::{FFT_BINS, NUM_Q};
use lactea_audio::{AnalysisMode, Analyzer, AudioFrame, ShaderFft};
use lactea_preset::BaseValues;
use lactea_render::{CompatRules, ShaderExt};
use std::sync::Arc;

/// The host's mouse, as MD3's `_c14` shader constant (`mouse`, `mouse_x` .. of the shader prelude) sees it. Preset
/// expressions cannot read it: MD3's expression grammar has no dotted identifiers, so `mouse.x` does not compile there.
///
/// Feed it with [`crate::Engine::set_mouse`] whenever the host learns something new (cursor move, button change);
/// the engine reads the latest value once per rendered frame and derives the one-frame "released" edge itself, so a
/// press and release between two rendered frames is invisible, exactly as in MD3.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MouseState {
    /// The cursor in the visualizer's client area as fractions of its size, `(px / width, py / height)` with the
    /// origin at the **top left** (y grows downwards), or `None` when the cursor is outside the area. Both
    /// coordinates are expected in `0..1`.
    pub position: Option<[f32; 2]>,
    /// The left button is down. MD3 reads the system-wide button state, so the host should report it even while
    /// the cursor is outside the window.
    pub left_down: bool,
}

/// The mouse of one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MouseFrame {
    /// `_c14` of the shaders: `x`, `y` (top origin), held, released.
    pub shader: [f32; 4],
}

/// What [`Md3Inputs::begin_frame`] samples for one frame.
pub(crate) struct Md3Frame {
    pub mouse: MouseFrame,
    /// The shader FFT after this frame's step (`None` unless `CompatRules::md3_shader_fft`).
    pub fft: Option<Arc<[f32; FFT_BINS]>>,
}

/// Engine-wide MD3 input state; lives as long as the engine and is never reset by a preset change.
pub(crate) struct Md3Inputs {
    mouse: MouseState,
    /// The button level of the previous frame's sample (initially up).
    prev_down: bool,
    fft: ShaderFft,
    /// `FFTAttack` / `FFTDecay`: process-global in MD3, set by every preset load, never blended.
    attack: f32,
    decay: f32,
}

impl Md3Inputs {
    pub fn new() -> Self {
        Self {
            mouse: MouseState::default(),
            prev_down: false,
            fft: ShaderFft::new(),
            attack: 0.5,
            decay: 0.7,
        }
    }

    pub fn set_mouse(&mut self, m: MouseState) {
        self.mouse = m;
    }

    /// A preset was imported: `FFTAttack` / `FFTDecay` become its file values (0.5 / 0.7 when it has no such keys),
    /// whatever the previous preset had. MD3 stores them unclamped; a non-finite value (`FFTAttack=nan`) would
    /// poison the persistent FFT state for the rest of the run, so it is replaced by the default instead.
    pub fn on_preset_loaded(&mut self, rules: &CompatRules, base: &BaseValues) {
        if rules.md3_shader_fft {
            let pick = |v: f32, d: f32| if v.is_finite() { v } else { d };
            self.attack = pick(base.fft_attack, 0.5);
            self.decay = pick(base.fft_decay, 0.7);
        }
    }

    /// The sticky `(FFTAttack, FFTDecay)`.
    #[cfg(test)]
    pub fn fft_params(&self) -> (f32, f32) {
        (self.attack, self.decay)
    }

    /// Sample the frame's inputs: the mouse (always advances the release edge) and, once per rendered frame, one step
    /// of the shader FFT from the left spectrum of `audio`.
    pub fn begin_frame(&mut self, rules: &CompatRules, audio: &AudioFrame) -> Option<Md3Frame> {
        if !(rules.md3_extended_vars || rules.md3_shader_fft || rules.md3_shader_time) {
            return None;
        }
        let mouse = self.sample_mouse();
        let fft = rules
            .md3_shader_fft
            .then(|| Arc::new(*self.fft.step(&audio.spectrum[0], self.attack, self.decay)));
        Some(Md3Frame { mouse, fft })
    }

    fn sample_mouse(&mut self) -> MouseFrame {
        let m = self.mouse;
        // `((2 p / size - 1) * 0.5) + 0.5`, -1 outside; MD3 3.37 stores the bottom-origin y and derives the
        // shader's top-origin y as `1 - y`
        let (x, yb) = match m.position {
            Some([u, v]) => (((2.0 * u - 1.0) * 0.5) + 0.5, ((1.0 - 2.0 * v) * 0.5) + 0.5),
            None => (-1.0, -1.0),
        };
        let ys = if yb == -1.0 { -1.0 } else { 1.0 - yb };
        let z = if m.left_down { 1.0 } else { 0.0 };
        let w = if !m.left_down && self.prev_down {
            1.0
        } else {
            0.0
        };
        self.prev_down = m.left_down;
        MouseFrame {
            shader: [x, ys, z, w],
        }
    }
}

/// The audio analyzer of an engine: MilkDrop 3's under `CompatRules::md3_audio_analysis` (with the configured
/// `VolumeSensitivity`), otherwise the projectM one.
pub(crate) fn new_analyzer(cfg: &EngineConfig) -> Analyzer {
    if cfg.compat.rules().md3_audio_analysis {
        let mut a = Analyzer::with_mode(cfg.sample_rate, AnalysisMode::Md3);
        a.set_volume_sensitivity(cfg.volume_sensitivity);
        a
    } else {
        Analyzer::new(cfg.sample_rate)
    }
}

/// The MD3 shader inputs of one state's passes: its own `q33..q64` (read from `pf` after its per-frame code), the
/// frame's mouse, the time since `start` (this state's preset start) and, for the new state, the shader FFT.
pub(crate) fn shader_ext(
    rules: &CompatRules,
    md3: &Md3Frame,
    pf: &PerFrame,
    start: f64,
    now: f64,
    with_fft: bool,
) -> ShaderExt {
    let mut ext = ShaderExt::default();
    if rules.md3_extended_vars {
        ext.mouse = md3.mouse.shader;
        for (i, q) in ext.q_hi.iter_mut().enumerate() {
            if NUM_Q + i < pf.v.q.len() {
                *q = pf.q(NUM_Q + i) as f32;
            }
        }
    }
    if rules.md3_shader_time {
        ext.time = Some(now - start);
    }
    if with_fft {
        ext.fft = md3.fft.clone();
    }
    ext
}

#[cfg(test)]
mod tests {
    use super::*;
    use lactea_render::Compat;

    fn sample(i: &mut Md3Inputs, pos: Option<[f32; 2]>, down: bool) -> MouseFrame {
        i.set_mouse(MouseState {
            position: pos,
            left_down: down,
        });
        i.sample_mouse()
    }

    fn close(a: [f32; 4], b: [f32; 4]) {
        for k in 0..4 {
            assert!((a[k] - b[k]).abs() < 1e-5, "{a:?} != {b:?}");
        }
    }

    /// Spec C 8.2: a 1920x1080 client.
    #[test]
    fn mouse_values_in_the_shader_convention() {
        let (w, h) = (1920.0f32, 1080.0f32);
        let at = |px: f32, py: f32| Some([px / w, py / h]);
        let mut i = Md3Inputs::new();
        let m = sample(&mut i, at(960.0, 540.0), false);
        close(m.shader, [0.5, 0.5, 0.0, 0.0]);
        let m = sample(&mut i, at(0.0, 0.0), false);
        close(m.shader, [0.0, 0.0, 0.0, 0.0]);
        let m = sample(&mut i, at(480.0, 270.0), true);
        close(m.shader, [0.25, 0.25, 1.0, 0.0]);
        let m = sample(&mut i, at(1919.0, 1079.0), false);
        close(m.shader, [0.99948, 0.99907, 0.0, 1.0]);
        // outside: (-1, -1), the button state is not suppressed
        let m = sample(&mut i, None, true);
        assert_eq!(m.shader, [-1.0, -1.0, 1.0, 0.0]);
    }

    #[test]
    fn released_is_one_frame_wide_and_a_missed_click_is_invisible() {
        let mut i = Md3Inputs::new();
        let p = Some([0.3, 0.6]);
        let zw = |m: MouseFrame| (m.shader[2], m.shader[3]);
        assert_eq!(zw(sample(&mut i, p, false)), (0.0, 0.0));
        assert_eq!(zw(sample(&mut i, p, true)), (1.0, 0.0));
        assert_eq!(zw(sample(&mut i, p, true)), (1.0, 0.0));
        assert_eq!(zw(sample(&mut i, p, false)), (0.0, 1.0));
        assert_eq!(zw(sample(&mut i, p, false)), (0.0, 0.0));
        // press + release between two samples: the engine only ever sees the last state
        i.set_mouse(MouseState {
            position: p,
            left_down: true,
        });
        i.set_mouse(MouseState {
            position: p,
            left_down: false,
        });
        assert_eq!(zw(i.sample_mouse()), (0.0, 0.0));
    }

    #[test]
    fn fft_params_follow_every_import_and_default_without_keys() {
        let rules = Compat::MilkDrop3.rules();
        let mut i = Md3Inputs::new();
        assert_eq!(i.fft_params(), (0.5, 0.7));
        let mut base = BaseValues::default();
        base.fft_attack = 0.9;
        base.fft_decay = 0.5;
        i.on_preset_loaded(&rules, &base);
        assert_eq!(i.fft_params(), (0.9, 0.5));
        i.on_preset_loaded(&rules, &BaseValues::default());
        assert_eq!(i.fft_params(), (0.5, 0.7));
        // other compat modes never touch them
        let mut j = Md3Inputs::new();
        j.on_preset_loaded(&Compat::MilkDrop2.rules(), &base);
        assert_eq!(j.fft_params(), (0.5, 0.7));
    }

    #[test]
    fn nothing_is_sampled_outside_milkdrop3() {
        let audio = AudioFrame::default();
        let mut i = Md3Inputs::new();
        assert!(i.begin_frame(&Compat::MilkDrop2.rules(), &audio).is_none());
        assert!(
            i.begin_frame(&Compat::MilkDrop3.rules(), &audio)
                .is_some_and(|f| f.fft.is_some())
        );
    }
}
