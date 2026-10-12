// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: (MIT OR Apache-2.0)

//! The per-frame audio-envelope / beat-latch block behind the `_mxd3qxa.._d` / `_mxd3qsa.._c`
//! shader uniforms (`MilkDrop 3`). It is recovered from the reference binary (undocumented) and
//! reproduced here so a renderer can feed those uniforms without shipping any shader content.
//!
//! The block is a small stateful DSP chain driven once per frame by the six audio levels
//! (`bass`/`mid`/`treb` and their `_att` averages), the frame rate and the elapsed time:
//!
//! * four fixed constants (`1, 2, 3, 27`);
//! * an energy accumulator and a bass accumulator (both linear in time under silence);
//! * a beat timer that re-arms on a loud frame and gates a handful of `rand()` draws;
//! * three beat detectors, each with its own envelope, threshold ratio, minimum delay and wrapped
//!   beat counter (`mod 4`, `mod 3`, `mod 4`);
//! * nine "time since the last reset" latches: every beat resets the one latch whose counter window
//!   it lands in, every other latch (and every frame) just adds `1 / fps`;
//! * three decaying pulse smoothers (half-life `0.15 s`) that share the first detector's windows.
//!
//! All state starts at zero, so the three envelopes ramp up from nothing on the first frames and
//! each detector fires several times during that start-up transient even under silence. That
//! transient (not any per-latch increment) is what spreads the nine latches over a fraction of a
//! second, and its exact shape follows the wall-clock `elapsed` of the first ~35 frames. The only
//! non-reproducible part is the `rand()` stream (distribution-only).
//!
//! See `~/M0Rf30/md3-re/specs/E9-mxd3q-beat-latch.md` for the derivation, the addresses and the
//! constants.

/// `DAT_14010a104`: the scale the signed draws multiply by (`2^-15`, not `1 / 32767`).
const RAND_SCALE: f32 = 1.0 / 32768.0;

/// Half-width of every pulse window around a beat-counter value (`DAT_14010cbb4`).
const WINDOW_HALF_WIDTH: f32 = 0.5;

/// Half-life of the pulse smoothers in seconds (`DAT_14010ecc8`): `powf(0.5, dt / 0.15)` per frame.
const SMOOTHER_HALF_LIFE: f32 = 0.15;

/// `bass_att` above this saturates the three pulse smoothers (`DAT_14010eb98`).
const BASS_SATURATION: f32 = 5.0;

/// The beat-counter values that reset each latch, per detector (`|phase - k| < 0.5`): detector 1
/// (`mod 4`, latches `0x908c4c/50/54`), detector 2 (`mod 3`, `0x908c60/64/68`), detector 3
/// (`mod 4`, `0x908c74/78/7c`). The first detector's windows also drive the pulse smoothers.
const LATCH_PHASES: [[&[f32]; 3]; 3] = [
    [&[0.0], &[2.0], &[1.0, 3.0]],
    [&[2.0], &[1.0], &[0.0]],
    [&[1.0], &[0.0, 2.0], &[3.0]],
];

/// The seven float4 uniforms the block produces, in upload order.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Mxd3q {
    /// `_mxd3qxa` — the four fixed constants.
    pub qxa: [f32; 4],
    /// `_mxd3qxb` — energy accumulator, bass accumulator, two random draws.
    pub qxb: [f32; 4],
    /// `_mxd3qxc` — four random draws.
    pub qxc: [f32; 4],
    /// `_mxd3qxd` — the held signed draw, the accumulated second signed draw and two slots the
    /// reference never writes (always `0`).
    pub qxd: [f32; 4],
    /// `_mxd3qsa` — the three pulse smoothers and the first latch.
    pub qsa: [f32; 4],
    /// `_mxd3qsb` — four latches.
    pub qsb: [f32; 4],
    /// `_mxd3qsc` — four latches.
    pub qsc: [f32; 4],
}

/// The six audio levels the block consumes per frame (MD3's silence floor is `1.0`).
#[derive(Clone, Copy, Debug, Default)]
pub struct Mxd3qAudio {
    /// `imm_rel[0]` (immediate bass).
    pub bass: f32,
    /// `imm_rel[1]` (immediate mid).
    pub mid: f32,
    /// `imm_rel[2]` (immediate treble).
    pub treb: f32,
    /// `avg_rel[0]` (smoothed bass).
    pub bass_att: f32,
    /// `avg_rel[1]` (smoothed mid).
    pub mid_att: f32,
    /// `avg_rel[2]` (smoothed treble).
    pub treb_att: f32,
}

/// One beat detector: the time of its last beat and its wrapped beat counter.
#[derive(Clone, Copy, Debug, Default)]
struct Detector {
    last_beat: f32,
    phase: f32,
}

impl Detector {
    /// Fire when `triggered` and more than `delay` seconds passed since the last beat, then advance
    /// the beat counter `fmodf(fired + phase, modulus)`.
    fn fire(&mut self, triggered: bool, elapsed: f32, delay: f32, modulus: f32) -> bool {
        let fired = triggered && elapsed - self.last_beat > delay;
        if fired {
            self.last_beat = elapsed;
        }
        self.phase = (f32::from(fired) + self.phase) % modulus;
        fired
    }
}

/// The persistent per-engine state of the block (survives preset changes, like the reference).
/// The doc of every field gives the offset of the reference's float it mirrors.
#[derive(Clone, Debug, Default)]
pub struct Mxd3qBlock {
    rng: u64,
    /// `0x908bcc`.
    bass_acc: f32,
    /// `0x908bd0`.
    energy: f32,
    /// `0x908bd4`; dips to `-0.0166` for one frame when it runs out.
    beat_timer: f32,
    /// `0x908bdc..=0x908bf0`.
    rands: [f32; 6],
    /// `0x908c34`, `0x908c38`: the first detector's envelope and its slower follower.
    env: [f32; 2],
    /// `0x908c3c`: slow average of `treb_att` (second detector).
    treb_avg: f32,
    /// `0x908c40`: slow average of `mid_att` (third detector).
    mid_avg: f32,
    /// `0x908c44/48`, `0x908c58/5c`, `0x908c6c/70`.
    detectors: [Detector; 3],
    /// `0x908c80..=0x908c88`.
    pulses: [f32; 3],
    /// `0x908c4c/50/54/60/64/68/74/78/7c`.
    latches: [f32; 9],
    /// `0x908c8c`: the held signed draw.
    kick: f32,
    /// `0x908c90`: the held step of the second signed draw.
    drift_step: f32,
    /// `0x908c94`: the running sum of `drift_step`, one add per frame.
    drift: f32,
    /// The output of the last [`Self::step`] (all zero before the first).
    last: Mxd3q,
}

impl Mxd3qBlock {
    /// A 15-bit LCG in the spirit of the C runtime `rand()` (deterministic, seeded).
    fn next_rand(&mut self) -> f32 {
        self.rng = self.rng.wrapping_mul(2_147_013).wrapping_add(2_531_011);
        ((self.rng >> 17) & 0x7fff) as f32
    }

    /// `rand() * 2^-15 <= 0.5 ? -rand() : rand()` (two draws either way).
    fn next_signed(&mut self) -> f32 {
        let negative = self.next_rand() * RAND_SCALE <= 0.5;
        let v = self.next_rand();
        if negative { -v } else { v }
    }

    /// Advance one frame. `elapsed` is the reference's `time - m_fStartTime` in seconds; `fps` is the
    /// reported frame rate (a non-positive rate gives `dt = 0` and freezes the bass accumulator, like
    /// the reference); `q64` is the preset's `q64` variable after its per-frame code, whose positive
    /// part is the first detector's minimum delay between beats (`0x140032213`: `fmaxf(q64, 0)`; a
    /// preset that never writes it leaves `0`, so that detector fires on every frame its envelope test
    /// passes). Returns the seven uniform float4s.
    pub fn step(&mut self, audio: &Mxd3qAudio, fps: f32, elapsed: f32, q64: f32) -> Mxd3q {
        let (bass, mid, treb) = (audio.bass, audio.mid, audio.treb);
        let (bass_att, mid_att, treb_att) = (audio.bass_att, audio.mid_att, audio.treb_att);
        let dt = if fps > 0.0 { 1.0 / fps } else { 0.0 };
        let asum = mid_att + bass_att + treb_att;
        let ssum = mid + bass + treb;

        // energy + bass accumulators (0x140031eba..0x140031f10; linear in time under silence)
        let e = asum * 0.2;
        self.energy += e * e * 0.1;
        let qx5 = self.energy * 0.4 + elapsed * 0.05;
        if fps > 0.0 {
            self.bass_acc += bass / fps;
        }

        // beat timer (0x140031f1c..0x140031f77): `T = (0 <= T) ? T - 0.0166 : 0`, re-armed to 0.25 on a
        // loud frame (bass + mid + treb > 4.25) when that decrement went below 0. The decrement is
        // stored, so the timer rests alternating between `0` and `-0.0166` and a loud frame arms it
        // on every other frame at best.
        let timer = if self.beat_timer >= 0.0 {
            self.beat_timer - 0.0166
        } else {
            0.0
        };
        self.beat_timer = timer;
        if ssum > 4.25 && timer < 0.0 {
            self.beat_timer = 0.25;
        }

        // random draws, only on the frame the timer was armed (0x140031f7d..0x140032134)
        if self.beat_timer > 0.24 {
            self.rands[0] = self.next_rand() % 6.0 + 8.0;
            self.rands[1] = self.next_rand() % 6.0 + 3.0;
            self.rands[2] = self.next_rand() % 10.0;
            self.rands[3] = self.next_rand() / 32767.0;
            self.rands[4] = self.next_rand() / 32767.0;
            self.rands[5] = self.next_rand() / 32767.0;
            self.kick = self.next_signed() * RAND_SCALE * 10.0;
            self.drift_step = self.next_signed() * RAND_SCALE * 0.05;
        }
        self.drift += self.drift_step;

        // detector 1 envelope (attack 0.3 / decay 0.7, 0x14003219e..0x14003220b)
        self.env[0] = asum * 0.7 / 3.0 + self.env[0] * 0.3;
        self.env[1] = self.env[0] * 0.3 + self.env[1] * 0.7;
        // detectors 2 and 3 follow `treb_att` / `mid_att` with a 0.95 / 0.05 average
        // (0x1400324ec..0x14003253a)
        self.treb_avg = self.treb_avg * 0.95 + treb_att * 0.05;
        self.mid_avg = mid_att * 0.05 + self.mid_avg * 0.95;

        // beats: ratio test and minimum delay per detector (0x140032225, 0x14003254f, 0x140032677)
        let fired = [
            self.detectors[0].fire(self.env[0] > self.env[1] * 1.15, elapsed, q64.max(0.0), 4.0),
            self.detectors[1].fire(treb_att > self.treb_avg * 1.2, elapsed, 0.15, 3.0),
            self.detectors[2].fire(mid_att > self.mid_avg * 1.2, elapsed, 0.2, 4.0),
        ];
        // which latch each beat resets: the beat counter must sit in the latch's window
        let resets: [[bool; 3]; 3] = std::array::from_fn(|d| {
            let phase = self.detectors[d].phase;
            std::array::from_fn(|k| {
                fired[d]
                    && LATCH_PHASES[d][k]
                        .iter()
                        .any(|&p| (phase - p).abs() < WINDOW_HALF_WIDTH)
            })
        });

        // pulse smoothers (0x140032340..0x140032443): `max(s * 0.5^(dt / 0.15), pulse * flag)`, and a
        // `bass_att > 5` frame saturates all three
        let decay = 0.5_f32.powf(dt / SMOOTHER_HALF_LIFE);
        let hot = f32::from(bass_att > BASS_SATURATION);
        for (s, &reset) in self.pulses.iter_mut().zip(&resets[0]) {
            *s = (decay * *s).max(f32::from(reset)).max(hot);
        }

        // latches (0x14003245f..0x1400327b4): reset on their pulse, else add dt
        for (latch, &reset) in self.latches.iter_mut().zip(resets.iter().flatten()) {
            *latch = if reset { 0.0 } else { *latch + dt };
        }

        let l = &self.latches;
        let out = Mxd3q {
            qxa: [1.0, 2.0, 3.0, 27.0],
            qxb: [qx5, self.bass_acc, self.rands[0], self.rands[1]],
            qxc: [self.rands[2], self.rands[3], self.rands[4], self.rands[5]],
            qxd: [self.kick, self.drift, 0.0, 0.0],
            qsa: [self.pulses[0], self.pulses[1], self.pulses[2], l[0]],
            qsb: [l[1], l[2], l[3], l[4]],
            qsc: [l[5], l[6], l[7], l[8]],
        };
        self.last = out;
        out
    }

    /// The output of the last [`Self::step`]: what the uniforms hold until the next one.
    pub fn last(&self) -> Mxd3q {
        self.last
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32, eps: f32) {
        assert!((a - b).abs() <= eps, "{a} !~ {b}");
    }

    fn silence() -> Mxd3qAudio {
        Mxd3qAudio {
            bass: 1.0,
            mid: 1.0,
            treb: 1.0,
            bass_att: 1.0,
            mid_att: 1.0,
            treb_att: 1.0,
        }
    }

    /// The nine latches in `L0..L8` order (`qsa.w`, `qsb.xyzw`, `qsc.xyzw`).
    fn latches(q: &Mxd3q) -> [f32; 9] {
        let [a, b, c] = [q.qsa, q.qsb, q.qsc];
        [a[3], b[0], b[1], b[2], b[3], c[0], c[1], c[2], c[3]]
    }

    /// `frames` silent frames at the reported rate `fps`; frame `f` happens `first + f / fps` seconds
    /// after the start (`first` is the first frame's `elapsed`).
    fn silent_run(fps: f32, first: f32, frames: usize) -> (Mxd3qBlock, Mxd3q) {
        let mut b = Mxd3qBlock::default();
        let mut q = Mxd3q::default();
        for f in 0..frames {
            q = b.step(&silence(), fps, first + f as f32 / fps, 0.0);
        }
        (b, q)
    }

    #[test]
    fn constants_are_fixed() {
        let mut b = Mxd3qBlock::default();
        let q = b.step(&silence(), 60.0, 1.0 / 60.0, 0.0);
        assert_eq!(q.qxa, [1.0, 2.0, 3.0, 27.0]);
    }

    #[test]
    fn zero_fps_keeps_the_accumulators_finite() {
        // the block is never reset, so one non-finite value would poison every later preset
        let mut b = Mxd3qBlock::default();
        for f in 0..10 {
            let q = b.step(&silence(), 0.0, f as f32, 0.0);
            assert!(q.qxb.iter().chain(&q.qsb).all(|v| v.is_finite()), "{q:?}");
            // dt is 0 and the bass accumulator is frozen, like the reference
            assert_eq!(q.qxb[1], 0.0);
            assert_eq!(latches(&q), [0.0; 9]);
        }
    }

    #[test]
    fn a_loud_frame_draws_on_the_frame_it_arms_the_timer() {
        let loud = Mxd3qAudio {
            bass: 2.0,
            mid: 2.0,
            treb: 2.0,
            ..silence()
        };
        let q = Mxd3qBlock::default().step(&loud, 60.0, 1.0 / 60.0, 0.0);
        assert!((8.0..14.0).contains(&q.qxb[2]), "{:?}", q.qxb);
        assert!((3.0..9.0).contains(&q.qxb[3]), "{:?}", q.qxb);
    }

    #[test]
    fn the_timer_dips_below_zero_so_a_loud_frame_arms_every_other_frame_at_rest() {
        // root cause: the reference stores `T - 0.0166` even when it is negative; the next frame then
        // clamps to 0 (not below it), so a loud frame cannot arm straight after a rest frame
        let loud = Mxd3qAudio {
            bass: 2.0,
            mid: 2.0,
            treb: 2.0,
            ..silence()
        };
        let mut b = Mxd3qBlock::default();
        b.step(&silence(), 60.0, 0.0, 0.0); // T: 0 -> -0.0166
        let q = b.step(&loud, 60.0, 1.0 / 60.0, 0.0); // T: -0.0166 -> 0, not below 0
        assert_eq!(q.qxb[2..], [0.0, 0.0], "armed too early: {:?}", q.qxb);
        let q = b.step(&loud, 60.0, 2.0 / 60.0, 0.0); // T: 0 -> -0.0166 -> armed
        assert!((8.0..14.0).contains(&q.qxb[2]), "{:?}", q.qxb);
    }

    #[test]
    fn the_signed_draws_are_held_between_beats_and_the_second_one_accumulates() {
        // root cause: both draws are gated on the armed timer (they are 0 under silence, as probed);
        // the first is held, the second feeds a running sum that gains its step on every frame
        let (_, q) = silent_run(60.0, 1.0, 50);
        assert_eq!(q.qxd, [0.0; 4]);

        let loud = Mxd3qAudio {
            bass: 2.0,
            mid: 2.0,
            treb: 2.0,
            ..silence()
        };
        let mut b = Mxd3qBlock::default();
        let armed = b.step(&loud, 60.0, 1.0 / 60.0, 0.0).qxd;
        assert!(armed[0] != 0.0 && armed[0].abs() < 10.0, "{armed:?}");
        let (mut prev, mut steps) = (armed[1], vec![]);
        for f in 2..6 {
            let q = b.step(&silence(), 60.0, f as f32 / 60.0, 0.0);
            assert_eq!(q.qxd[0], armed[0], "the first draw is held");
            assert_eq!(q.qxd[2..], [0.0, 0.0], "xd.zw are never written");
            steps.push(q.qxd[1] - prev);
            prev = q.qxd[1];
        }
        assert!(steps[0] != 0.0 && steps.iter().all(|&s| (s - steps[0]).abs() < 1e-6));
    }

    #[test]
    fn detector_one_cascades_through_the_start_up_transient() {
        // root cause of the latch spread: all state starts at 0, so the envelope ratio A0 > 1.15 A1
        // holds for the first 6 frames; with a minimum delay of 0 the first detector beats on every
        // one of them. Its counter walks 1, 2, 3, 0, 1, 2 and resets (L2) (L1) (L2) (L0) (L2) (L1).
        let dt = 1.0 / 60.0;
        let (mut b, q) = silent_run(60.0, dt, 6);
        let l = latches(&q);
        approx(l[0], 2.0 * dt, 1e-6); // reset on beat 4
        approx(l[1], 0.0, 0.0); // reset on beat 6 (this frame)
        approx(l[2], dt, 1e-6); // reset on beat 5
        for &v in &l[3..] {
            approx(v, 6.0 * dt, 1e-6); // detectors 2 and 3 have not seen 0.15 s yet
        }
        // the smoothers follow the same windows: phase 0 (frame 4), 2 (frame 6), 1 or 3 (frame 5)
        let decay = 0.5_f32.powf(dt / 0.15);
        approx(q.qsa[0], decay * decay, 1e-6);
        approx(q.qsa[1], 1.0, 0.0);
        approx(q.qsa[2], decay, 1e-6);
        // the envelopes have caught up on frame 7: no beat, every latch simply adds dt
        let q = b.step(&silence(), 60.0, 7.0 * dt, 0.0);
        approx(latches(&q)[1], dt, 1e-6);
        approx(latches(&q)[0], 3.0 * dt, 1e-6);
    }

    #[test]
    fn q64_is_the_first_detectors_minimum_delay() {
        // `fmaxf(q64, 0)`: a delay longer than the elapsed time suppresses the whole cascade
        let dt = 1.0 / 60.0;
        let run = |q64: f32| {
            let mut b = Mxd3qBlock::default();
            let mut q = Mxd3q::default();
            for f in 1..=10 {
                q = b.step(&silence(), 60.0, f as f32 * dt, q64);
            }
            latches(&q)
        };
        for v in &run(1.0)[..3] {
            approx(*v, 10.0 * dt, 1e-6); // never reset
        }
        assert_eq!(run(-5.0), run(0.0), "a negative q64 clamps to 0");
        assert!(run(0.0)[..3].iter().all(|&v| v < 10.0 * dt));
    }

    #[test]
    fn detectors_two_and_three_walk_their_latches_in_counter_order() {
        // detector 2 (treb_att vs its average, mod 3) resets L4, L3, L5, L4, L3; detector 3 (mid_att,
        // mod 4) resets L6, L7, L8, L7, L6. Beats are 0.3 s apart, clear of both minimum delays.
        let dt = 1.0 / 60.0;
        let loud = Mxd3qAudio {
            mid_att: 4.0,
            treb_att: 4.0,
            ..silence()
        };
        let (d2, d3) = ([4, 3, 5, 4, 3], [6, 7, 8, 7, 6]);
        let mut b = Mxd3qBlock::default();
        let mut age = [0usize; 9]; // frames since each latch's last reset
        for k in 0..5 {
            let q = b.step(&loud, 60.0, 1.0 + 0.3 * k as f32, 0.0);
            for (i, a) in age.iter_mut().enumerate().skip(3) {
                *a = if i == d2[k] || i == d3[k] { 0 } else { *a + 1 };
            }
            for (i, v) in latches(&q).into_iter().enumerate().skip(3) {
                approx(v, age[i] as f32 * dt, 1e-5);
            }
        }
    }

    #[test]
    fn a_hot_bass_att_saturates_the_pulse_smoothers_that_then_decay_with_a_150ms_half_life() {
        // `elapsed` stays 0, so no detector fires and only the `bass_att > 5` rule feeds the smoothers
        let dt = 1.0 / 60.0;
        let mut b = Mxd3qBlock::default();
        let warm = Mxd3qAudio {
            bass_att: 5.0,
            ..silence()
        };
        assert_eq!(
            b.step(&warm, 60.0, 0.0, 0.0).qsa[..3],
            [0.0; 3],
            "5 is not above 5"
        );
        let hot = Mxd3qAudio {
            bass_att: 5.01,
            ..silence()
        };
        assert_eq!(b.step(&hot, 60.0, 0.0, 0.0).qsa[..3], [1.0; 3]);
        for n in 1..=9 {
            let q = b.step(&silence(), 60.0, 0.0, 0.0);
            for s in &q.qsa[..3] {
                approx(*s, 0.5_f32.powf(n as f32 * dt / 0.15), 1e-5);
            }
            if n == 9 {
                approx(q.qsa[0], 0.5, 1e-5); // 9 frames at 60 fps are one half-life
            }
        }
    }

    #[test]
    fn every_latch_adds_one_frame_period_per_frame() {
        // root cause of the "different growth rates" in the probe: there are none; two runs only
        // differ in their frame timing. Once the start-up transient is over each latch gains `1 / fps`.
        let fps = 56.34_f32;
        let (mut b, q) = silent_run(fps, 0.0, 400);
        let next = b.step(&silence(), fps, 400.0 / fps, 0.0);
        for (a, c) in latches(&q).into_iter().zip(latches(&next)) {
            approx(c - a, 1.0 / fps, 1e-5);
        }
    }

    #[test]
    fn silence_matches_the_probe() {
        // The oracle shows each cell as `v / 16` in an 8-bit channel (`ret = m.xyz * 0.0625`), so a
        // reading is good to +-0.031 and a difference of two to +-0.063; the reported fps also jitters
        // while the model runs at a constant one. 0.1 covers both.
        //
        // Both probe runs are silent frame loops at ~56 fps. What differs between them is the
        // wall-clock of the first frames (`first`, the first frame's `elapsed`): it decides which frame
        // each detector's first beat lands on, hence how far the latches are spread. The probe did not
        // record it; 0 s (run A) and 0.06 s (run B) are inside the bands that reproduce each run to
        // within `TOL` (A: 0 s, up to 0.04 s at the edge of `TOL`; B: 0.045..=0.115 s).
        const TOL: f32 = 0.1;

        // Run A, `.xyz` cells (`ret = m.xyz`): at the 8 s shot (B = 6.78 s) and the 10 s shot (B = 8.72
        // s). The energy and bass accumulators fix the frame count and the mean rate of each: 382
        // frames at 56.34 fps, then 484 frames at 55.5 fps (the reported rate drifts down).
        struct Shot {
            fps: f32,
            frames: usize,
            bass: f32,
            energy: f32,
            sb: [f32; 3],
            sc: [f32; 3],
        }
        for shot in [
            Shot {
                fps: 56.34,
                frames: 382,
                bass: 6.78,
                energy: 5.84,
                sb: [6.71, 6.59, 6.40],
                sc: [6.27, 6.53, 6.27],
            },
            Shot {
                fps: 55.5,
                frames: 484,
                bass: 8.72,
                energy: 7.40,
                sb: [8.66, 8.60, 8.35],
                sc: [8.22, 8.47, 8.28],
            },
        ] {
            let (_, q) = silent_run(shot.fps, 0.0, shot.frames);
            assert_eq!(q.qxa, [1.0, 2.0, 3.0, 27.0]);
            approx(q.qxb[0], shot.energy, TOL); // energy accumulator + elapsed
            approx(q.qxb[1], shot.bass, TOL); // bass accumulator = sum of 1 / fps
            assert_eq!(q.qxb[2..], [0.0, 0.0]); // the draws wait for a loud frame
            assert_eq!(q.qxc, [0.0; 4]);
            assert_eq!(q.qxd, [0.0; 4]);
            for v in &q.qsa[..3] {
                assert!(v.abs() < 1e-6, "the smoothers have decayed: {:?}", q.qsa);
            }
            for (got, want) in q.qsb[..3].iter().zip(shot.sb) {
                approx(*got, want, TOL);
            }
            for (got, want) in q.qsc[..3].iter().zip(shot.sc) {
                approx(*got, want, TOL);
            }
        }

        // Run B, `.w` cells (`ret = m.www`): L0 = qsa.w, L4 = qsb.w and L8 = qsc.w, at the 8 s and
        // 10 s shots (the run advanced 1.45 s between them). L4 and L8 are equal in the probe.
        for (frames, want) in [(498, [8.78, 8.22, 8.22]), (580, [10.23, 9.66, 9.66])] {
            let (_, q) = silent_run(56.34, 0.06, frames);
            for (got, want) in [q.qsa[3], q.qsb[3], q.qsc[3]].into_iter().zip(want) {
                approx(got, want, TOL);
            }
        }
    }
}
