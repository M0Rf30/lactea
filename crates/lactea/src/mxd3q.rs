// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: (MIT OR Apache-2.0)

//! The per-frame audio-envelope / beat-latch block behind the `_mxd3qxa.._d` / `_mxd3qsa.._c`
//! shader uniforms (`MilkDrop 3`). It is reconstructed from the reference binary and reproduced
//! here so a renderer can feed those uniforms without shipping any shader content.
//!
//! The block is a small stateful DSP chain driven once per frame by the six audio levels
//! (`bass`/`mid`/`treb` and their `_att` averages), the frame rate and the elapsed time:
//!
//! * four fixed constants (`1, 2, 3, 27`);
//! * an energy accumulator and a bass accumulator (both linear in time under silence);
//! * a beat detector (smoothed envelopes, a `1.15` threshold and a `0.15 s` minimum delay) whose
//!   flag resets the latches (the reference's three `mod 4 / 3 / 4` phase accumulators and their
//!   pulse windows, which reset each latch on a different beat, are approximated by one flag);
//! * nine "time since last beat" latches;
//! * a handful of `rand()` draws.
//!
//! Under silence every accumulator reduces to a linear recurrence in time, so the silent output is
//! deterministic and the latches track elapsed time. The only non-reproducible part is the
//! `rand()` stream (distribution-only).
//!
//! See `~/M0Rf30/md3-re/specs/E9-mxd3q-beat-latch.md` for the derivation and the constants.

/// The seven float4 uniforms the block produces, in upload order.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Mxd3q {
    /// `_mxd3qxa` — the four fixed constants.
    pub qxa: [f32; 4],
    /// `_mxd3qxb` — energy accumulator, bass accumulator, two random draws.
    pub qxb: [f32; 4],
    /// `_mxd3qxc` — four random draws.
    pub qxc: [f32; 4],
    /// `_mxd3qxd` — the signed random draw and three reserved slots.
    pub qxd: [f32; 4],
    /// `_mxd3qsa` — three reserved slots and a beat latch.
    pub qsa: [f32; 4],
    /// `_mxd3qsb` — four beat latches.
    pub qsb: [f32; 4],
    /// `_mxd3qsc` — four beat latches.
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

/// The persistent per-engine state of the block (survives preset changes, like the reference).
#[derive(Clone, Debug, Default)]
pub struct Mxd3qBlock {
    rng: u64,
    energy: f32,
    bass_acc: f32,
    beat_timer: f32,
    rands: [f32; 6],
    env: [f32; 4],
    last_beat: f32,
    latches: [f32; 9],
}

impl Mxd3qBlock {
    /// A 15-bit LCG in the spirit of the C runtime `rand()` (deterministic, seeded).
    fn next_rand(&mut self) -> f32 {
        self.rng = self.rng.wrapping_mul(2_147_013).wrapping_add(2_531_011);
        ((self.rng >> 17) & 0x7fff) as f32
    }

    /// Advance one frame. `elapsed` is seconds since the preset started; `fps` is the reported
    /// frame rate (a non-positive rate counts as 1 fps, so the accumulators stay finite). Returns
    /// the seven uniform float4s.
    pub fn step(&mut self, audio: &Mxd3qAudio, fps: f32, elapsed: f32) -> Mxd3q {
        let (bass, mid, treb) = (audio.bass, audio.mid, audio.treb);
        let (bass_att, mid_att, treb_att) = (audio.bass_att, audio.mid_att, audio.treb_att);
        let dt = 1.0 / fps.max(1.0);
        let asum = bass_att + mid_att + treb_att;
        let ssum = bass + mid + treb;

        // energy + bass accumulators (linear in time under silence)
        self.energy += (asum * 0.2) * (asum * 0.2) * 0.1;
        self.bass_acc += bass * dt;
        // QX5 = E·0.4 + t·0.05 (spec §4 step 2)
        let qx5 = self.energy * 0.4 + elapsed * 0.05;

        // beat timer (spec §4 step 4, before the draws that read it): decrement, and re-arm on a
        // loud frame (bass + mid + treb > 4.25) only when the decrement crossed below 0
        let timer_prev = self.beat_timer;
        self.beat_timer = (self.beat_timer - 0.0166).max(0.0);
        if ssum > 4.25 && timer_prev - 0.0166 < 0.0 {
            self.beat_timer = 0.25;
        }

        // random draws (refresh only on the frame the timer was armed, so they stay 0 under silence)
        if self.beat_timer > 0.24 {
            self.rands[0] = self.next_rand() % 6.0 + 8.0;
            self.rands[1] = self.next_rand() % 6.0 + 3.0;
            self.rands[2] = self.next_rand() % 10.0;
            self.rands[3] = self.next_rand() / 32767.0;
            self.rands[4] = self.next_rand() / 32767.0;
            self.rands[5] = self.next_rand() / 32767.0;
        }
        let signed = if self.next_rand() / 32767.0 <= 0.5 {
            -self.next_rand()
        } else {
            self.next_rand()
        } / 32767.0
            * 10.0;

        // smoothed envelopes (attack 0.3 / decay 0.7)
        self.env[0] = asum * 0.7 / 3.0 + self.env[0] * 0.3;
        self.env[1] = self.env[0] * 0.3 + self.env[1] * 0.7;
        self.env[2] = treb * 0.7 / 3.0 + self.env[2] * 0.3;
        self.env[3] = bass * 0.7 / 3.0 + self.env[3] * 0.3;

        // beat flag: A0 > A1*1.15 with a 0.15 s minimum delay
        let flag = if self.env[0] > self.env[1] * 1.15 && elapsed - self.last_beat > 0.15 {
            self.last_beat = elapsed;
            1.0
        } else {
            0.0
        };

        // latches: "time since the last beat". Under silence (no beat) they all accumulate `dt` and
        // track elapsed time; a beat resets them to zero.
        for latch in &mut self.latches {
            *latch = if flag > 0.0 { 0.0 } else { *latch + dt };
        }

        Mxd3q {
            qxa: [1.0, 2.0, 3.0, 27.0],
            qxb: [qx5, self.bass_acc, self.rands[0], self.rands[1]],
            qxc: [self.rands[2], self.rands[3], self.rands[4], self.rands[5]],
            qxd: [signed, 0.0, 0.0, 0.0],
            qsa: [0.0, 0.0, 0.0, self.latches[0]],
            qsb: [
                self.latches[1],
                self.latches[2],
                self.latches[3],
                self.latches[4],
            ],
            qsc: [
                self.latches[5],
                self.latches[6],
                self.latches[7],
                self.latches[8],
            ],
        }
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

    #[test]
    fn constants_are_fixed() {
        let mut b = Mxd3qBlock::default();
        let q = b.step(&silence(), 60.0, 1.0 / 60.0);
        assert_eq!(q.qxa, [1.0, 2.0, 3.0, 27.0]);
    }

    #[test]
    fn zero_fps_keeps_the_accumulators_finite() {
        // the block is never reset, so one non-finite value would poison every later preset
        let mut b = Mxd3qBlock::default();
        for f in 0..10 {
            let q = b.step(&silence(), 0.0, f as f32);
            assert!(q.qxb.iter().chain(&q.qsb).all(|v| v.is_finite()), "{q:?}");
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
        let q = Mxd3qBlock::default().step(&loud, 60.0, 1.0 / 60.0);
        assert!((8.0..14.0).contains(&q.qxb[2]), "{:?}", q.qxb);
        assert!((3.0..9.0).contains(&q.qxb[3]), "{:?}", q.qxb);
    }

    #[test]
    fn silence_matches_the_probe() {
        // Silent frame loop at ~56 fps for ~6.78 s. The oracle measures qxb = (5.84, 6.78, 0, 0)
        // and nine latches scattered over 6.27..8.78 (≈ elapsed time). The energy/bass accumulators
        // are exact; the latches are modeled as plain dt accumulators, so they all equal elapsed
        // time here — the per-register spread of the oracle is an unverified approximation.
        let mut b = Mxd3qBlock::default();
        let (fps, seconds) = (56.34_f32, 6.78_f32);
        let n = (fps * seconds) as usize;
        let mut q = Mxd3q::default();
        for f in 0..n {
            q = b.step(&silence(), fps, (f + 1) as f32 / fps);
        }
        approx(q.qxb[0], 5.84, 0.1); // energy accumulator
        approx(q.qxb[1], 6.78, 0.1); // bass accumulator
        approx(q.qxb[2], 0.0, 0.0); // randoms stay 0 under silence
        approx(q.qxb[3], 0.0, 0.0);
        // all nine latches track elapsed time under silence
        let all = q
            .qsa
            .iter()
            .chain(&q.qsb)
            .chain(&q.qsc)
            .skip(3) // qsa.xyz are reserved
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(all.len(), 9);
        for v in all {
            approx(v, seconds, 0.2);
        }
    }
}
