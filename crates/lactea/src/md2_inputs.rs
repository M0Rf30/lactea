//! BeatDrop's host inputs under `Compat::MilkDrop2` (`CompatRules::md2_input_vars` / `md2_shader_inputs`): the mouse
//! and the arrow-key flags, the wall clock and the shader FFT, as the expressions (`mousex .. totalseconds`) and the
//! shaders (`_c14` / `_c15` / `_c16`, `sampler_fft` / `sampler_wave`) see them.
//!
//! Like the MilkDrop 3 inputs ([`crate::md3_inputs`]) they are sampled **once per rendered frame** at the top of
//! [`crate::Engine::render`] and shared by every state drawn that frame. The engine does no I/O: the host sets the
//! mouse ([`crate::Engine::set_mouse`]), the arrow keys ([`crate::Engine::set_ctrl_keys`]) and the wall clock
//! ([`crate::Engine::set_date_time`]).

use crate::md3_inputs::MouseState;
use lactea_audio::{AudioFrame, BeatDropShaderFft, SHADER_FFT_BINS, SHADER_WAVE_SAMPLES};
use lactea_render::BeatDropTexture;
use std::sync::Arc;

/// The wall clock BeatDrop reads with `GetLocalTime`. The engine never looks at the system clock (a library must be
/// deterministic); until the host calls [`crate::Engine::set_date_time`] it derives the time of day from the frame
/// clock instead (see [`crate::Engine::set_date_time`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DateTime {
    /// Four-digit year (`year`).
    pub year: u16,
    /// 1 ..= 12 (`month`).
    pub month: u8,
    /// 1 ..= 31 (`day`).
    pub day: u8,
    /// 1 = Monday .. 7 = Sunday (`weekday`; BeatDrop maps Windows' Sunday = 0 to 7).
    pub weekday: u8,
    /// 0 ..= 23 (`hour`).
    pub hour: u8,
    /// 0 ..= 59 (`minute`).
    pub minute: u8,
    /// 0 ..= 59 (`second`).
    pub second: u8,
    /// 0 ..= 999 (`milliseconds`).
    pub millisecond: u16,
}

impl DateTime {
    /// Whether every field is in the range it has on a real clock.
    pub fn is_valid(&self) -> bool {
        (1..=12).contains(&self.month)
            && (1..=31).contains(&self.day)
            && (1..=7).contains(&self.weekday)
            && self.hour < 24
            && self.minute < 60
            && self.second < 60
            && self.millisecond < 1000
    }

    /// `totalSeconds`: `hour * 3600 + minute * 60 + second + millisecond * .001`, in `f32` like BeatDrop.
    fn total_seconds(&self) -> f32 {
        f32::from(self.hour) * 3600.0
            + f32::from(self.minute) * 60.0
            + f32::from(self.second)
            + f32::from(self.millisecond) * 0.001
    }

    /// The time of day of a frame clock reading (`seconds` since the start of an unspecified day), with a zero
    /// date: what the engine shows until the host supplies the real clock.
    fn from_frame_time(seconds: f64) -> Self {
        let day = seconds.rem_euclid(86_400.0);
        let whole = day.floor();
        let n = whole as u32;
        DateTime {
            hour: (n / 3600) as u8,
            minute: (n / 60 % 60) as u8,
            second: (n % 60) as u8,
            millisecond: (((day - whole) * 1000.0) as u16).min(999),
            ..DateTime::default()
        }
    }
}

/// The arrow keys held together with Ctrl (`ctrllt ctrlrt ctrldn ctrlup`): BeatDrop sets them from Ctrl + arrow key
/// events. The host reports the current state; the engine samples it once per rendered frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CtrlKeys {
    /// Ctrl + Left (`ctrllt`).
    pub left: bool,
    /// Ctrl + Right (`ctrlrt`).
    pub right: bool,
    /// Ctrl + Down (`ctrldn`).
    pub down: bool,
    /// Ctrl + Up (`ctrlup`).
    pub up: bool,
}

/// The values the expression contexts get for one frame (`milkdropfs.cpp:688-705`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Md2Vars {
    /// `mousex`, `mousey` (bottom-origin, `-1` outside the window), `mousedown`, `mouseclick`.
    pub mouse: [f64; 4],
    /// `ctrllt`, `ctrlrt`, `ctrldn`, `ctrlup`.
    pub ctrl: [f64; 4],
    /// `year month day weekday hour minute second milliseconds totalseconds`.
    pub date: [f64; 9],
}

/// What [`Md2Inputs::begin_frame`] samples for one frame.
pub(crate) struct Md2Frame {
    pub vars: Md2Vars,
    /// `_c14` of BeatDrop's shaders (`milkdropfs.cpp:5317`): `(x, y, ±lastX, ±lastY)`.
    pub mouse: [f32; 4],
    /// `_c15` (`hour, minute, second, totalSeconds`) and `_c16` (`year, month, day, weekday`).
    pub time_date: [[f32; 4]; 2],
    /// The spectrum / waveform texture (`CompatRules::md2_shader_inputs`).
    pub texture: Option<Arc<BeatDropTexture>>,
}

/// Engine-wide BeatDrop input state; lives as long as the engine and is never reset by a preset change.
pub(crate) struct Md2Inputs {
    mouse: MouseState,
    ctrl: CtrlKeys,
    date: Option<DateTime>,
    /// The button level of the previous frame's sample (initially up).
    prev_down: bool,
    /// `m_lastMouseX` / `m_lastMouseY`: where the last press happened (`y` top-origin).
    last_click: [f32; 2],
    fft: Option<Box<BeatDropShaderFft>>,
}

impl Md2Inputs {
    pub fn new() -> Self {
        Self {
            mouse: MouseState::default(),
            ctrl: CtrlKeys::default(),
            date: None,
            prev_down: false,
            last_click: [0.0, 0.0],
            fft: None,
        }
    }

    pub fn set_mouse(&mut self, m: MouseState) {
        self.mouse = m;
    }

    pub fn set_ctrl_keys(&mut self, k: CtrlKeys) {
        self.ctrl = k;
    }

    pub fn set_date_time(&mut self, d: Option<DateTime>) {
        self.date = d;
    }

    /// Feed the shader FFT's PCM history (created on first use: other modes never pay for it).
    pub fn push_pcm(&mut self, interleaved: &[f32], channels: usize) {
        self.fft
            .get_or_insert_with(|| Box::new(BeatDropShaderFft::new()))
            .push_pcm_f32(interleaved, channels);
    }

    /// Sample the frame: mouse and keys, the clock, and (`shader_fft`) one step of the shader FFT with the state's
    /// `FFTAttack` / `FFTDecay`.
    pub fn begin_frame(
        &mut self,
        audio: &AudioFrame,
        fps: f32,
        time: f64,
        (attack, decay): (f32, f32),
        shader_fft: bool,
    ) -> Md2Frame {
        // `m_mouseX = ((2 x / w - 1) / 2) + .5` and `m_mouseY = ((1 - 2 y / h) / 2) + .5` inside the window, -1 outside
        let (x, y) = match self.mouse.position {
            Some([u, v]) => (((2.0 * u - 1.0) / 2.0) + 0.5, ((1.0 - 2.0 * v) / 2.0) + 0.5),
            None => (-1.0, -1.0),
        };
        let down = self.mouse.left_down;
        let click = down && !self.prev_down;
        self.prev_down = down;
        if click {
            // the press is recorded where the cursor was: `m_lastMouseY = -m_mouseY + 1`
            self.last_click = [x, -y + 1.0];
        }
        let [lx, ly] = self.last_click;
        let mouse = [
            x,
            if y != -1.0 { -y + 1.0 } else { -1.0 },
            if down { lx } else { -lx },
            if click { ly } else { -ly },
        ];

        let date = self.date.unwrap_or_else(|| DateTime::from_frame_time(time));
        let total = date.total_seconds();
        let flag = |b: bool| if b { 1.0 } else { 0.0 };
        let vars = Md2Vars {
            mouse: [f64::from(x), f64::from(y), flag(down), flag(click)],
            ctrl: [
                flag(self.ctrl.left),
                flag(self.ctrl.right),
                flag(self.ctrl.down),
                flag(self.ctrl.up),
            ],
            date: [
                f64::from(date.year),
                f64::from(date.month),
                f64::from(date.day),
                f64::from(date.weekday),
                f64::from(date.hour),
                f64::from(date.minute),
                f64::from(date.second),
                f64::from(date.millisecond),
                f64::from(total),
            ],
        };
        let time_date = [
            [
                f32::from(date.hour),
                f32::from(date.minute),
                f32::from(date.second),
                total,
            ],
            [
                f32::from(date.year),
                f32::from(date.month),
                f32::from(date.day),
                f32::from(date.weekday),
            ],
        ];

        let texture = shader_fft.then(|| {
            let fft = self
                .fft
                .get_or_insert_with(|| Box::new(BeatDropShaderFft::new()));
            fft.update(fps, attack, decay);
            let mut wave = [[0.0f32; SHADER_WAVE_SAMPLES]; 2];
            for (row, src) in wave.iter_mut().zip(&audio.wave) {
                for (d, s) in row.iter_mut().zip(src) {
                    *d = s / 128.0;
                }
            }
            let mut tex = BeatDropTexture {
                spectrum: [0.0; SHADER_FFT_BINS],
                peak: [0.0; SHADER_FFT_BINS],
                wave,
            };
            tex.spectrum.copy_from_slice(fft.smoothed());
            tex.peak.copy_from_slice(fft.peak());
            Arc::new(tex)
        });

        Md2Frame {
            vars,
            mouse,
            time_date,
            texture,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(i: &mut Md2Inputs, pos: Option<[f32; 2]>, down: bool) -> Md2Frame {
        i.set_mouse(MouseState {
            position: pos,
            left_down: down,
        });
        i.begin_frame(&AudioFrame::default(), 60.0, 0.0, (0.5, 0.7), false)
    }

    /// The texture layout is spelled out in three crates (the analyser, the renderer's texture, the shader lowering);
    /// they must agree or `get_fft` / `get_wave` read the wrong texels.
    #[test]
    fn the_beatdrop_texture_layout_agrees_across_crates() {
        assert_eq!(lactea_hlsl::BEATDROP_FFT_BINS, SHADER_FFT_BINS);
        assert_eq!(lactea_render::BEATDROP_FFT_BINS, SHADER_FFT_BINS);
        assert_eq!(lactea_hlsl::BEATDROP_WAVE_SAMPLES, SHADER_WAVE_SAMPLES);
        assert_eq!(lactea_render::BEATDROP_WAVE_SAMPLES, SHADER_WAVE_SAMPLES);
        assert_eq!(lactea_render::BEATDROP_ROWS, 4);
    }

    /// `mousex` / `mousey` are bottom-origin, `mouseclick` lasts one frame, `_c14` carries the press position with
    /// the sign of "still held" / "clicked this frame".
    #[test]
    fn mouse_vars_and_c14() {
        let mut i = Md2Inputs::new();
        let f = frame(&mut i, None, false);
        assert_eq!(f.vars.mouse, [-1.0, -1.0, 0.0, 0.0]);
        assert_eq!(f.mouse, [-1.0, -1.0, 0.0, 0.0]);

        let f = frame(&mut i, Some([0.25, 0.25]), true);
        // the cursor is 1/4 from the left and 1/4 from the top: y is 3/4 bottom-origin
        assert_eq!(f.vars.mouse, [0.25, 0.75, 1.0, 1.0]);
        assert_eq!(f.mouse, [0.25, 0.25, 0.25, 0.25]);

        let f = frame(&mut i, Some([0.5, 0.5]), true);
        assert_eq!(
            f.vars.mouse,
            [0.5, 0.5, 1.0, 0.0],
            "no second click while held"
        );
        assert_eq!(
            f.mouse,
            [0.5, 0.5, 0.25, -0.25],
            "held: +lastX; no click: -lastY"
        );

        let f = frame(&mut i, Some([0.5, 0.5]), false);
        assert_eq!(f.vars.mouse[2], 0.0);
        assert_eq!(f.mouse[2], -0.25, "released: -lastX");
    }

    /// Unset: the time of day follows the frame clock; set: the host's values, with `totalseconds` derived.
    #[test]
    fn clock_follows_the_host_or_the_frame_time() {
        let mut i = Md2Inputs::new();
        let f = i.begin_frame(&AudioFrame::default(), 60.0, 3723.5, (0.5, 0.7), false);
        assert_eq!(f.time_date[0], [1.0, 2.0, 3.0, 3723.5]);
        assert_eq!(f.time_date[1], [0.0; 4]);

        i.set_date_time(Some(DateTime {
            year: 2026,
            month: 10,
            day: 13,
            weekday: 2,
            hour: 23,
            minute: 59,
            second: 58,
            millisecond: 250,
        }));
        let f = i.begin_frame(&AudioFrame::default(), 60.0, 0.0, (0.5, 0.7), false);
        assert_eq!(f.time_date[1], [2026.0, 10.0, 13.0, 2.0]);
        assert_eq!(
            f.vars.date[4..],
            [23.0, 59.0, 58.0, 250.0, f64::from(86_398.25f32)]
        );
    }
}
