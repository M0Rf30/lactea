//! Teacher forcing (fidelity ladder rung 3): render frame `n` from the oracle's state of frame
//! `n-1` so errors cannot accumulate and each diff points at one pass.

use lactea_audio::AudioFrame;
use lactea_eel::Context;
use lactea_render::PassRandom;
use std::collections::BTreeMap;

/// Oracle state to impose on the **next** rendered frame ([`crate::Engine::force_next_frame`]).
///
/// The engine still runs every expression (so megabuf and other state evolve), then overwrites
/// what the oracle dumped:
/// * every per-frame variable present in the per-frame context (builtins, `q1..q32`, user
///   variables, `reg00..reg99`) — the values *after* the per-frame equations;
/// * `audio.*` (bass/mid/treb/vol and the `_att` values used by shaders and the main wave);
/// * `hue_offset.0..3`;
/// * `warp.*` / `comp.*`: `rand_frame`, `rand_preset` and the 24 `rot_*` matrices of each shader pass.
///
/// Dotted names that are not expression variables are consumed here, the rest is applied by name
/// (case-insensitive).
#[derive(Clone, Debug, Default)]
pub struct ForcedFrame {
    /// Contents of the oracle's `vars/NNNNN.json` for this frame.
    pub vars: BTreeMap<String, f64>,
    /// The oracle's `main` image of the previous frame (RGBA8, `width*height*4`, row 0 = top):
    /// what this frame's warp samples. `None` leaves the engine's own previous image.
    pub prev_main: Option<Vec<u8>>,
    /// Diagnostic: the oracle's `main` image of **this** frame, used as the composite pass's
    /// `sampler_main` instead of ours (the `main` stage dump stays ours). Isolates the
    /// composite shader from `main` errors. `None` = use our own image.
    pub comp_main: Option<Vec<u8>>,
}

impl ForcedFrame {
    pub(crate) fn override_audio(&self, a: &mut AudioFrame) {
        let get = |k: &str| self.vars.get(k).map(|v| *v as f32);
        if let Some(v) = get("audio.bass") {
            a.bass = v;
        }
        if let Some(v) = get("audio.mid") {
            a.mid = v;
        }
        if let Some(v) = get("audio.treb") {
            a.treb = v;
        }
        if let Some(v) = get("audio.vol") {
            a.vol = v;
        }
        if let Some(v) = get("audio.bass_att") {
            a.bass_att = v;
        }
        if let Some(v) = get("audio.mid_att") {
            a.mid_att = v;
        }
        if let Some(v) = get("audio.treb_att") {
            a.treb_att = v;
        }
        if let Some(v) = get("audio.vol_att") {
            a.vol_att = v;
        }
    }

    pub(crate) fn apply_vars(&self, ctx: &mut Context) {
        for (name, value) in &self.vars {
            if name.contains('.') {
                continue;
            }
            if let Some(v) = ctx.find_var(name) {
                ctx.set(v, *value);
            }
        }
    }

    pub(crate) fn hue_offsets(&self) -> Option<[f32; 4]> {
        let mut h = [0.0f32; 4];
        for (i, slot) in h.iter_mut().enumerate() {
            *slot = *self.vars.get(&format!("hue_offset.{i}"))? as f32;
        }
        Some(h)
    }

    /// `prefix` is `"warp."` or `"comp."`. `None` when the dump has no such pass.
    pub(crate) fn pass_random(&self, prefix: &str) -> Option<PassRandom> {
        let xyzw = ['x', 'y', 'z', 'w'];
        let mut rand_frame = [0.0f32; 4];
        let mut rand_preset = [0.0f32; 4];
        for k in 0..4 {
            rand_frame[k] = *self.vars.get(&format!("{prefix}rand_frame.{}", xyzw[k]))? as f32;
            rand_preset[k] = *self.vars.get(&format!("{prefix}rand_preset.{}", xyzw[k]))? as f32;
        }
        let names = [
            "rot_s1",
            "rot_s2",
            "rot_s3",
            "rot_s4",
            "rot_d1",
            "rot_d2",
            "rot_d3",
            "rot_d4",
            "rot_f1",
            "rot_f2",
            "rot_f3",
            "rot_f4",
            "rot_vf1",
            "rot_vf2",
            "rot_vf3",
            "rot_vf4",
            "rot_uf1",
            "rot_uf2",
            "rot_uf3",
            "rot_uf4",
            "rot_rand1",
            "rot_rand2",
            "rot_rand3",
            "rot_rand4",
        ];
        let mut rot = Box::new([[[0.0f32; 4]; 3]; 24]);
        let mut complete = true;
        for (m, name) in names.iter().enumerate() {
            for col in 0..3 {
                for row in 0..4 {
                    match self.vars.get(&format!("{prefix}{name}.{}", col * 4 + row)) {
                        Some(v) => rot[m][col][row] = *v as f32,
                        None => complete = false,
                    }
                }
            }
        }
        Some(PassRandom {
            rand_frame,
            rand_preset,
            rot: complete.then_some(rot),
        })
    }
}
