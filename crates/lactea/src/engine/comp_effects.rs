//! MD3 `AutoHardCutMode` inside the engine: the per-frame driver, the cut counter the hosts consume and the
//! composite-shader effect injection of mode 7 (`docs/md3.md`, "Auto hard cuts").

use super::*;
use crate::auto_cut::{Effect, toggle_effect};

impl Engine {
    /// The `AutoHardCutMode` that was set (0 = off), whatever the compat mode; see
    /// [`Engine::auto_hard_cut_active`].
    pub fn auto_hard_cut_mode(&self) -> u8 {
        self.auto_cut.mode()
    }

    /// Set MD3's `AutoHardCutMode`: 0 = off, 1..=7 see [`crate::auto_cut`]; larger values are clamped to 7.
    /// Takes effect on the next frame. Without [`Compat::MilkDrop3`] it is stored but never acts.
    pub fn set_auto_hard_cut_mode(&mut self, mode: u8) {
        self.auto_cut.set_mode(mode);
        self.cfg.auto_hard_cut_mode = self.auto_cut.mode();
    }

    /// Whether an auto hard cut mode is on *and* the compat mode supports it
    /// (`CompatRules::md3_auto_hard_cut`).
    pub fn auto_hard_cut_active(&self) -> bool {
        self.cfg.compat.rules().md3_auto_hard_cut && self.auto_cut.mode() > 0
    }

    /// The instant preset changes the auto hard cut asked for since the last call (mode 4 can ask for two in
    /// one frame). The host loads another preset with [`Transition::Cut`]; with a
    /// [`PresetControl`](crate::PresetControl) pass the count to
    /// [`PresetControl::update_with_auto_cuts`](crate::PresetControl::update_with_auto_cuts).
    pub fn take_auto_cuts(&mut self) -> u32 {
        std::mem::take(&mut self.pending_auto_cuts)
    }

    /// Start of every frame (MD3 runs the block at the top of its render function): feed the auto cut with the
    /// frame clock and the `bass` / `treb` the previous frame's per-frame code left in the running preset.
    pub(super) fn auto_cut_step(&mut self, now: f64) {
        if !self.auto_hard_cut_active() {
            return;
        }
        let Some(act) = self.active.as_ref() else {
            return;
        };
        let pf = &act.loaded.pf;
        let (bass, treb) = (pf.ctx.get(pf.v.bass), pf.ctx.get(pf.v.treb));
        let out = self.auto_cut.update(now, bass, treb);
        self.pending_auto_cuts = self.pending_auto_cuts.saturating_add(u32::from(out.cuts));
        if let Some(effect) = out.inject {
            self.inject_comp_effect(effect);
        }
    }

    /// Toggle `effect` in the running preset's composite shader (MD3 mode 7, see
    /// [`toggle_effect`] for the text rules) and rebuild the shader. The text lives in the loaded preset, so
    /// effects accumulate until the next preset load; injecting an effect that is already present removes it.
    ///
    /// A preset without composite-shader text (`PSVERSION_COMP < 2`, or no `comp_N` lines) first gets MD3's
    /// default composite shader generated from its legacy parameters (gamma, video echo, hue shader, brighten /
    /// darken / solarize / invert) and PS version 4. If the text has no insertion point or the changed shader does
    /// not translate / compile, nothing changes. Returns whether the effect was applied. MilkDrop 2 family only.
    pub fn inject_comp_effect(&mut self, effect: Effect) -> bool {
        if !self.cfg.compat.md2_family() {
            return false;
        }
        let Some(mut slot) = self.active.take() else {
            return false;
        };
        let applied = self.inject_into(&mut slot, effect);
        self.active = Some(slot);
        applied
    }

    fn inject_into(&mut self, slot: &mut Slot, effect: Effect) -> bool {
        let p = &slot.loaded.preset;
        let user = p
            .comp_shader
            .as_deref()
            .filter(|t| !code_to_source(t).trim().is_empty());
        let ps = p.comp_ps_version;
        let base_text = match user {
            Some(t) if ps >= 2 => t.to_string(),
            _ => default_comp_shader(
                p.base.gamma_adj,
                p.base.video_echo_alpha,
                p.base.video_echo_zoom,
                p.base.video_echo_orientation,
                p.base.shader,
                p.base.brighten,
                p.base.darken,
                p.base.solarize,
                p.base.invert,
            )
            .replace('\n', "\u{1}"),
        };
        let new_ps = if ps < 2 { 4 } else { ps };
        let Some(new_text) = toggle_effect(&base_text, effect) else {
            return false;
        };

        // translate through the load path (same options as the preset's own shaders); warp is skipped (PS 0)
        let mut tmp = Preset::default();
        tmp.base = p.base.clone();
        tmp.comp_shader = Some(new_text.clone());
        tmp.comp_ps_version = new_ps;
        let mut report = PresetReport::default();
        let (_, comp) = self.build_shaders(&tmp, &mut report);
        let Some(comp) = comp.filter(|_| report.comp == ShaderOutcome::User) else {
            return false;
        };
        let iface = comp.interface.clone();
        // on a rejected module the renderer keeps the previous composite shader
        if slot.renderer.set_comp_shader(Some(comp)).is_err() {
            return false;
        }
        self.resolve_textures(
            &mut slot.renderer,
            std::slice::from_ref(&iface),
            &mut report,
        );

        let seed = self.cfg.seed;
        let l = &mut slot.loaded;
        l.preset.comp_shader = Some(new_text);
        l.preset.comp_ps_version = new_ps;
        l.has_comp = true;
        // a preset that had no composite shader needs the shader random state its passes draw from
        if self.cfg.compat.rules().shared_shader_rand {
            l.warp_stream
                .get_or_insert_with(|| ShaderStream::new(seed, "shader_warp"));
        } else {
            l.comp_stream
                .get_or_insert_with(|| ShaderStream::new(seed, "shader_comp"));
        }
        true
    }
}
