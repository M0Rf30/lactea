//! MilkDrop 3 `AutoHardCutMode` 0..7 (MD3 3.37, `docs/md3.md` "Auto hard cuts"): beat-driven preset
//! changes and composite-shader effect injection.
//!
//! MD3 watches the `bass` / `treb` values the *previous* frame's per-frame code left in the running
//! preset and, depending on the mode, asks for an instant (blend time 0) preset change or injects one
//! random effect into the composite shader:
//!
//! | mode | fires when (`t` = seconds since the last event) | does |
//! |---|---|---|
//! | 0 | never | nothing |
//! | 1 | `bass > 1.5 && t >= 0.2` | cut |
//! | 2 | `treb > 2.9 && t >= 0.5` | cut |
//! | 3 | `treb > 2.9 && t >= 1.0` | cut |
//! | 4 | `treb > 2.9`, then `t >= 3.0` **and** (independently, same old `t`) `t >= 1.0 && treb > 8.0` | 0, 1 or 2 cuts |
//! | 5 | `treb > 2.9 && t >= 5.0` | cut |
//! | 6 | `bass > 1.5`, no minimum delay | cut |
//! | 7 | `treb > 2.9 && t >= 0.5` | inject one effect, the preset stays |
//!
//! The README of MD3 promises other numbers (mode 6 loads `Bass/WHITE.milk` for `bass > 4.9`, mode 7
//! waits 1.5 s); the 3.37 binary has neither, and this module follows the binary.
//!
//! [`AutoCut`] is the pure decision logic: it is fed the clock and the two levels once per frame and
//! returns what to do ([`AutoCutOutcome`]). The engine drives it from [`Engine::render`](crate::Engine::render)
//! (only under [`CompatRules::md3_auto_hard_cut`](lactea_render::CompatRules)) and hands the cuts to the
//! host through [`Engine::take_auto_cuts`](crate::Engine::take_auto_cuts) /
//! [`PresetControl::update_with_auto_cuts`](crate::PresetControl::update_with_auto_cuts).
//!
//! Effects are text insertions into the composite shader source ([`toggle_effect`]), applied by
//! [`Engine::inject_comp_effect`](crate::Engine::inject_comp_effect).

use crate::rng::StreamRng;

/// Highest mode (`AutoHardCutMode` cycles `0..=7`).
pub const AUTO_HARD_CUT_MODES: u8 = 7;

/// `bass > 1.5` (modes 1 and 6).
const BASS_LEVEL: f64 = 1.5;
/// `treb > 2.9f`: MD3 compares against the *float* constant widened to double.
const TREB_LEVEL: f64 = 2.9f32 as f64;
/// `treb > 8.0` (the second cut of mode 4).
const TREB_LOUD: f64 = 8.0;

/// A composite-shader effect mode 7 can inject (MD3's "Solarize", "Split", ... helpers).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Effect {
    /// `ret = ret*(1-ret)*4;`
    Solarize,
    /// `uv = float2 (abs(uv.x-0.5), uv.y);`
    Split,
    /// `uv = 1- abs( frac((uv-.5)*1.5 * 0.5 ) * 2 - 1.0 );`
    Multiplicate,
    /// `uv = uv.yx;`
    Rotate,
    /// `uv = 1-uv;`
    Flip,
    /// `ret = 1 - ret;`
    Invert,
}

/// The effects mode 7 picks from, in MD3's `rand() % 6` order.
pub const MODE7_EFFECTS: [Effect; 6] = [
    Effect::Solarize,
    Effect::Split,
    Effect::Multiplicate,
    Effect::Rotate,
    Effect::Flip,
    Effect::Invert,
];

/// Where an effect's two lines go in the composite shader text.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Site {
    /// Right after the first `{` that follows `shader_body` (coordinate effects run before the sampling).
    BodyStart,
    /// Before the last `}` (colour effects run after the preset's own code).
    BodyEnd,
}

impl Effect {
    /// The comment line that marks the effect in the shader text (and makes a second injection remove it).
    pub fn marker(self) -> &'static str {
        match self {
            Effect::Solarize => "//MilkDrop3 Solarize Mode:",
            Effect::Split => "//MilkDrop3 Split Mode:",
            Effect::Multiplicate => "//MilkDrop3 Multiplicate Mode:",
            Effect::Rotate => "//MilkDrop3 Rotate Mode:",
            Effect::Flip => "//MilkDrop3 Flip Mode:",
            Effect::Invert => "//MilkDrop3 Invert Mode:",
        }
    }

    /// The HLSL line inserted under the marker.
    pub fn code(self) -> &'static str {
        match self {
            Effect::Solarize => "ret = ret*(1-ret)*4;",
            Effect::Split => "uv = float2 (abs(uv.x-0.5), uv.y);",
            Effect::Multiplicate => "uv = 1- abs( frac((uv-.5)*1.5 * 0.5 ) * 2 - 1.0 );",
            Effect::Rotate => "uv = uv.yx;",
            Effect::Flip => "uv = 1-uv;",
            Effect::Invert => "ret = 1 - ret;",
        }
    }

    fn site(self) -> Site {
        match self {
            Effect::Solarize | Effect::Invert => Site::BodyEnd,
            _ => Site::BodyStart,
        }
    }
}

/// Line separator of preset shader text (`LINEFEED_CONTROL_CHAR`).
const SEP: char = '\u{1}';

/// End (exclusive) of the line that contains `from`, including its terminator (`0x01` or `\n`), or the
/// end of the text.
fn line_end(s: &str, from: usize) -> usize {
    s[from..]
        .find([SEP, '\n'])
        .map_or(s.len(), |i| from + i + 1)
}

/// MD3's effect toggle on a preset's composite-shader text (lines separated by `0x01`).
///
/// * Marker present: the marker line and the code line after it are removed.
/// * Otherwise a coordinate effect is inserted as `<marker> 0x01 <code> 0x01` right after the first `{`
///   that follows `shader_body`; a colour effect replaces everything from the last `}` on with
///   `<marker> 0x01 <code> 0x01 } 0x01`.
///
/// Returns `None` when the text has no insertion point (no `shader_body {` / no `}`), so the caller
/// leaves the shader alone.
pub fn toggle_effect(src: &str, effect: Effect) -> Option<String> {
    let marker = effect.marker();
    if let Some(p) = src.find(marker) {
        let end = line_end(src, line_end(src, p));
        let mut out = String::with_capacity(src.len());
        out.push_str(&src[..p]);
        out.push_str(&src[end..]);
        return Some(out);
    }
    let block = format!("{marker}{SEP}{}{SEP}", effect.code());
    match effect.site() {
        Site::BodyStart => {
            let body = src.find("shader_body")?;
            let brace = body + src[body..].find('{')? + 1;
            let mut out = String::with_capacity(src.len() + block.len());
            out.push_str(&src[..brace]);
            out.push_str(&block);
            out.push_str(&src[brace..]);
            Some(out)
        }
        Site::BodyEnd => {
            let last = src.rfind('}')?;
            let mut out = String::with_capacity(last + block.len() + 2);
            out.push_str(&src[..last]);
            out.push_str(&block);
            out.push('}');
            out.push(SEP);
            Some(out)
        }
    }
}

/// What [`AutoCut::update`] decided for one frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AutoCutOutcome {
    /// Instant preset changes requested this frame (0, 1, or 2 in mode 4 when both conditions hold).
    pub cuts: u8,
    /// Mode 7: the effect to toggle in the composite shader (the preset does not change).
    pub inject: Option<Effect>,
}

/// State of the auto hard cut: mode, the time of the last event and the effect stream.
///
/// Deterministic: time is the caller's frame clock (seconds), never the wall clock, and the effect
/// choice comes from a seeded stream.
#[derive(Clone, Debug)]
pub struct AutoCut {
    mode: u8,
    /// Clock of the last event; `None` until the first frame the mode was active (MD3 initialises its
    /// static to "now" at the first evaluation and keeps it while the mode is switched off).
    last: Option<f64>,
    counter: u64,
    rng: StreamRng,
}

impl AutoCut {
    /// `mode` is clamped to `0..=7`; `seed` seeds the mode-7 effect stream.
    pub fn new(seed: u64, mode: u8) -> Self {
        Self {
            mode: mode.min(AUTO_HARD_CUT_MODES),
            last: None,
            counter: 0,
            rng: StreamRng::new(seed, "auto_cut_effect"),
        }
    }

    /// The current mode (0 = off).
    pub fn mode(&self) -> u8 {
        self.mode
    }

    /// Set the mode, clamped to `0..=7`. The time of the last event is kept, like MD3's F8 cycling.
    pub fn set_mode(&mut self, mode: u8) {
        self.mode = mode.min(AUTO_HARD_CUT_MODES);
    }

    /// Number of cuts requested so far (mode 7 injections do not count).
    pub fn cuts_total(&self) -> u64 {
        self.counter
    }

    /// One rendered frame at `now` seconds; `bass` / `treb` are the values the previous frame's per-frame
    /// code left in the running preset.
    ///
    /// A clock that jumps back (a restarted host clock) re-anchors the last event at `now`, so the
    /// minimum delays count from the restart instead of blocking the mode until the old time is reached.
    pub fn update(&mut self, now: f64, bass: f64, treb: f64) -> AutoCutOutcome {
        let mut out = AutoCutOutcome::default();
        if self.mode == 0 {
            return out;
        }
        let last = match self.last {
            Some(l) if l <= now => l,
            _ => now,
        };
        self.last = Some(last);
        // MD3 keeps the elapsed time as a float: `float(ns) / 1e9f`
        let t = f64::from((now - last) as f32);
        let treb_high = treb > TREB_LEVEL;
        match self.mode {
            1 => out.cuts = u8::from(bass > BASS_LEVEL && t >= 0.2),
            2 => out.cuts = u8::from(treb_high && t >= 0.5),
            3 => out.cuts = u8::from(treb_high && t >= 1.0),
            4 if treb_high => {
                // both conditions use the `t` of the frame start: one frame can cut twice
                out.cuts = u8::from(t >= 3.0) + u8::from(t >= 1.0 && treb > TREB_LOUD);
            }
            5 => out.cuts = u8::from(treb_high && t >= 5.0),
            6 => out.cuts = u8::from(bass > BASS_LEVEL),
            7 if treb_high && t >= 0.5 => {
                // `rand() % 6` of the C runtime (0..=32767)
                let r = ((self.rng.next64() >> 49) % MODE7_EFFECTS.len() as u64) as usize;
                out.inject = Some(MODE7_EFFECTS[r]);
                self.last = Some(now);
            }
            _ => {}
        }
        if out.cuts > 0 {
            self.last = Some(now);
            self.counter += u64::from(out.cuts);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f64 = 0.16;

    /// `(frame, outcome)` of every frame that does something, for series `treb(frame)` / `bass(frame)`.
    fn run(
        mode: u8,
        frames: usize,
        treb: impl Fn(usize) -> f64,
        bass: impl Fn(usize) -> f64,
    ) -> Vec<(usize, AutoCutOutcome)> {
        let mut a = AutoCut::new(1, mode);
        (0..frames)
            .filter_map(|f| {
                let o = a.update(f as f64 * DT, bass(f), treb(f));
                (o != AutoCutOutcome::default()).then_some((f, o))
            })
            .collect()
    }

    /// Input A of the spec's vectors (40 frames).
    fn input_a(mode: u8) -> Vec<(usize, AutoCutOutcome)> {
        run(
            mode,
            40,
            |f| match f {
                16 | 28 | 38 => 9.0,
                2 | 3 | 8 | 9 | 10 | 22 | 23 | 35 => 3.5,
                _ => 1.0,
            },
            |f| {
                if matches!(f, 3 | 4 | 5 | 10 | 11 | 25) {
                    2.0
                } else {
                    1.0
                }
            },
        )
    }

    fn frames(v: &[(usize, AutoCutOutcome)]) -> Vec<usize> {
        v.iter().map(|(f, _)| *f).collect()
    }

    fn cuts(v: &[(usize, AutoCutOutcome)]) -> Vec<u8> {
        v.iter().map(|(_, o)| o.cuts).collect()
    }

    #[test]
    fn mode_1_bass_with_a_200_ms_delay() {
        // frame 4 has bass > 1.5 but t = 0.16 s; frame 11 likewise
        assert_eq!(frames(&input_a(1)), [3, 5, 10, 25]);
    }

    #[test]
    fn mode_2_treble_with_a_500_ms_delay() {
        // frames 2 and 3 have a high treble but t < 0.5 s
        assert_eq!(frames(&input_a(2)), [8, 16, 22, 28, 35]);
    }

    #[test]
    fn mode_3_treble_with_a_1_s_delay() {
        // frame 22 has t = 0.96 s
        assert_eq!(frames(&input_a(3)), [8, 16, 23, 35]);
    }

    #[test]
    fn mode_4_only_the_loud_branch_fires_on_input_a() {
        // the 3.5 treble frames never reach t >= 3 s; the 9.0 frames cut through `t >= 1 && treb > 8`
        let v = input_a(4);
        assert_eq!(frames(&v), [16, 28, 38]);
        assert_eq!(cuts(&v), [1, 1, 1]);
    }

    #[test]
    fn mode_4_can_cut_twice_in_one_frame() {
        let v = run(
            4,
            60,
            |f| match f {
                30 | 55 => 9.0,
                2 | 20 => 3.5,
                _ => 1.0,
            },
            |_| 1.0,
        );
        // frame 2: t = 0.32; frame 20: t = 3.2 (branch a); frame 30: t = 1.6 (branch b);
        // frame 55: t = 4.0, both branches
        assert_eq!(frames(&v), [20, 30, 55]);
        assert_eq!(cuts(&v), [1, 1, 2]);
    }

    #[test]
    fn mode_5_treble_with_a_5_s_delay() {
        assert_eq!(frames(&input_a(5)), [35]);
        let v = run(
            5,
            40,
            |f| {
                if matches!(f, 10 | 20 | 33 | 34) {
                    3.0
                } else {
                    1.0
                }
            },
            |_| 1.0,
        );
        // frame 33: t = 5.28 s; frame 34: t = 0.16 s
        assert_eq!(frames(&v), [33]);
    }

    #[test]
    fn mode_6_cuts_on_every_loud_bass_frame() {
        assert_eq!(frames(&input_a(6)), [3, 4, 5, 10, 11, 25]);
    }

    #[test]
    fn mode_7_injects_without_cutting() {
        let v = input_a(7);
        assert_eq!(frames(&v), [8, 16, 22, 28, 35]);
        for (_, o) in &v {
            assert_eq!(o.cuts, 0);
            assert!(o.inject.is_some());
        }
    }

    #[test]
    fn mode_7_effect_choice_is_seeded_and_reaches_all_six() {
        let pick = |seed| {
            let mut a = AutoCut::new(seed, 7);
            (0..600)
                .filter_map(|f| a.update(f as f64, 1.0, 3.5).inject)
                .collect::<Vec<_>>()
        };
        let a = pick(7);
        assert_eq!(a, pick(7));
        for e in MODE7_EFFECTS {
            assert!(a.contains(&e), "{e:?} never picked");
        }
    }

    #[test]
    fn modes_off_and_no_audio_never_fire() {
        for mode in 0..=7 {
            // no audio: bass = treb = 1.0 forever
            assert!(run(mode, 400, |_| 1.0, |_| 1.0).is_empty(), "mode {mode}");
        }
        // mode 0 ignores loud input
        assert!(run(0, 40, |_| 9.0, |_| 9.0).is_empty());
    }

    #[test]
    fn thresholds_are_strict_and_the_treble_one_is_the_float_2_9() {
        // 2.9 as an f64 is below the float constant 2.9f widened to double
        assert!(run(2, 10, |_| 2.9, |_| 1.0).is_empty());
        assert_eq!(frames(&run(2, 10, |_| 2.91, |_| 1.0)), [4, 8]);
        assert!(run(1, 10, |_| 1.0, |_| 1.5).is_empty());
    }

    #[test]
    fn the_first_evaluation_anchors_the_timer_and_a_backwards_clock_re_anchors_it() {
        let mut a = AutoCut::new(1, 3);
        // enabled late: the timer starts at the first evaluation, not at clock 0
        assert_eq!(a.update(100.0, 1.0, 3.5).cuts, 0);
        assert_eq!(a.update(100.9, 1.0, 3.5).cuts, 0);
        assert_eq!(a.update(101.1, 1.0, 3.5).cuts, 1);
        // the host clock restarts
        assert_eq!(a.update(0.0, 1.0, 3.5).cuts, 0);
        assert_eq!(a.update(1.1, 1.0, 3.5).cuts, 1);
        assert_eq!(a.cuts_total(), 2);
    }

    #[test]
    fn a_mode_change_keeps_the_last_event() {
        let mut a = AutoCut::new(1, 2);
        assert_eq!(a.update(0.0, 1.0, 1.0).cuts, 0);
        a.set_mode(0);
        assert_eq!(a.update(5.0, 1.0, 9.0), AutoCutOutcome::default());
        a.set_mode(200);
        assert_eq!(a.mode(), 7);
        a.set_mode(3);
        // `last` is still 0: t = 6 s
        assert_eq!(a.update(6.0, 1.0, 9.0).cuts, 1);
    }

    #[test]
    fn effects_toggle_in_the_shader_text() {
        let src = "shader_body\u{1}{\u{1}ret = tex2D(sampler_main, uv).xyz;\u{1}}\u{1}";

        // coordinate effect: right after the first `{`
        let a = toggle_effect(src, Effect::Flip).unwrap();
        assert_eq!(
            a,
            "shader_body\u{1}{//MilkDrop3 Flip Mode:\u{1}uv = 1-uv;\u{1}\u{1}ret = tex2D(sampler_main, uv).xyz;\u{1}}\u{1}"
        );
        // second injection removes it again
        assert_eq!(toggle_effect(&a, Effect::Flip).unwrap(), src);

        // colour effect: before the last `}`
        let b = toggle_effect(src, Effect::Invert).unwrap();
        assert_eq!(
            b,
            "shader_body\u{1}{\u{1}ret = tex2D(sampler_main, uv).xyz;\u{1}//MilkDrop3 Invert Mode:\u{1}ret = 1 - ret;\u{1}}\u{1}"
        );
        assert_eq!(
            toggle_effect(&b, Effect::Invert).unwrap(),
            "shader_body\u{1}{\u{1}ret = tex2D(sampler_main, uv).xyz;\u{1}}\u{1}"
        );

        // effects accumulate, and each one toggles independently
        let c = toggle_effect(&b, Effect::Split).unwrap();
        assert!(c.contains("Split Mode") && c.contains("Invert Mode"));
        let d = toggle_effect(&c, Effect::Split).unwrap();
        assert_eq!(d, b);

        // `{` only after `shader_body`: an earlier brace does not count
        let pre = "float f(float x) { return x; }\u{1}shader_body\u{1}{\u{1}}\u{1}";
        let e = toggle_effect(pre, Effect::Rotate).unwrap();
        assert!(e.starts_with(
            "float f(float x) { return x; }\u{1}shader_body\u{1}{//MilkDrop3 Rotate Mode:"
        ));
    }

    #[test]
    fn effects_need_an_insertion_point() {
        assert_eq!(toggle_effect("ret = 1;", Effect::Flip), None);
        assert_eq!(toggle_effect("shader_body ret = 1;", Effect::Flip), None);
        assert_eq!(
            toggle_effect("shader_body { ret = 1;", Effect::Invert),
            None
        );
    }

    #[test]
    fn removal_takes_the_marker_line_and_the_code_line_only() {
        let s = "a;\u{1}//MilkDrop3 Rotate Mode:\u{1}uv = uv.yx;\u{1}b;\u{1}";
        assert_eq!(toggle_effect(s, Effect::Rotate).unwrap(), "a;\u{1}b;\u{1}");
        // marker without code line (truncated text)
        assert_eq!(
            toggle_effect("x;\u{1}//MilkDrop3 Rotate Mode:", Effect::Rotate).unwrap(),
            "x;\u{1}"
        );
    }

    #[test]
    fn injected_composite_shaders_still_translate() {
        use lactea_abi::PassKind;
        use lactea_hlsl::{Options, default_comp_shader, translate};
        let texts = [
            // MD3's generated default composite (what a preset without comp text gets)
            default_comp_shader(1.2, 0.5, 1.25, 1, 0.3, true, false, true, false, true)
                .replace('\n', "\u{1}"),
            "shader_body\u{1}{\u{1}  ret = tex2D(sampler_main, uv).xyz;\u{1}}\u{1}".to_string(),
            // comments, a helper function before the body, the closing brace on the code line
            "float3 tint(float3 c) { return c * 0.5; }\u{1}shader_body\u{1}{ // body\u{1}  float2 d = uv - 0.5;\u{1}  ret = tint(tex2D(sampler_main, uv + d * 0.1).xyz); }\u{1}"
                .to_string(),
        ];
        let opts = Options::default();
        for text in &texts {
            for effect in MODE7_EFFECTS {
                let once = toggle_effect(text, effect).unwrap();
                translate(PassKind::Comp, &once.replace(SEP, "\n"), &opts)
                    .unwrap_or_else(|e| panic!("{effect:?} on {text:?}: {e}"));
                // all six at once
                let mut all = text.clone();
                for e in MODE7_EFFECTS {
                    all = toggle_effect(&all, e).unwrap();
                }
                translate(PassKind::Comp, &all.replace(SEP, "\n"), &opts)
                    .unwrap_or_else(|e| panic!("all effects on {text:?}: {e}"));
            }
        }
    }
}
