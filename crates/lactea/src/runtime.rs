//! Expression contexts of a loaded preset and how projectM feeds them.
//!
//! Behaviour follows projectM: `PerFrameContext`, `PerPixelContext`,
//! `WaveformPerFrameContext` / `WaveformPerPointContext`, `ShapePerFrameContext` and
//! `MilkdropPreset::{PerFrameUpdate, CompileCodeAndRunInitExpressions}`.

use crate::EngineError;
use crate::vars::{PfVars, PpVars, ShVars, WfVars, WpVars};
use lactea_eel::{CompileError, Context, Dialect, GlobalMemory, Program};
use lactea_preset::{
    BaseValues, CustomShape, CustomWave, LINEFEED_CONTROL_CHAR, code_to_source,
    strip_linefeeds_and_comments,
};
use lactea_render::CompatRules;
use std::sync::Arc;

pub const NUM_Q: usize = 32;
/// `q1..q64` / `t1..t16` of MilkDrop 3: the capacity of every q / t array. A context registers [`NUM_Q`] q (and 8 t)
/// variables, or all of them after `extend_md3`; the loops follow the registered handles.
pub const MAX_Q: usize = lactea_abi::NUM_Q_MD3;
pub const MAX_T: usize = lactea_abi::NUM_T_MD3;

/// What the contexts see of the host: projectM's `RenderContext` + `FrameAudioData` (floats).
#[derive(Clone, Debug, Default)]
pub struct Env {
    pub time: f32,
    pub fps: f32,
    pub frame: u32,
    pub progress: f32,
    pub bass: f32,
    pub mid: f32,
    pub treb: f32,
    pub bass_att: f32,
    pub mid_att: f32,
    pub treb_att: f32,
    pub mesh: [u32; 2],
    pub pixels: [u32; 2],
    /// `renderContext.aspectX/Y` and their inverses.
    pub aspect: [f32; 2],
    pub inv_aspect: [f32; 2],
}

/// Compile `src` (a joined code block, see `lactea_preset`) into `ctx`; empty source gives `None` (projectM leaves
/// the handle null). A non-empty source that compiles to an empty program (comments only) is kept as `Some`.
///
/// `md2_join` (`CompatRules::md2_code_join`) selects MilkDrop 2's `StripLinefeedCharsAndComments` instead of the
/// `\n` join: a block that is only blanks and separators or strips to nothing compiles no code, like MD2's
/// `buf[0] == 0` test.
pub fn compile_opt(
    ctx: &mut Context,
    src: &str,
    md2_join: bool,
) -> Result<Option<Program>, CompileError> {
    let src = if md2_join {
        if src.chars().all(|c| c == ' ' || c == LINEFEED_CONTROL_CHAR) {
            return Ok(None);
        }
        strip_linefeeds_and_comments(src)
    } else {
        code_to_source(src)
    };
    if src.is_empty() {
        return Ok(None);
    }
    ctx.compile(&src).map(Some)
}

/// [`compile_opt`] for one named code part of a preset load, under the compat rules' code-failure policy.
///
/// A part that does not compile rejects the preset ([`EngineError::Compile`]), except with
/// `CompatRules::md3_drop_failed_code` (MilkDrop 3): the whole part is dropped, the reason is pushed to `warnings`
/// and the load goes on without that code (`Ok(None)`, exactly like a part the preset does not have).
pub fn compile_part(
    ctx: &mut Context,
    src: &str,
    rules: &CompatRules,
    what: &str,
    warnings: &mut Vec<String>,
) -> Result<Option<Program>, EngineError> {
    match compile_opt(ctx, src, rules.md2_code_join) {
        Ok(p) => Ok(p),
        Err(e) if rules.md3_drop_failed_code => {
            warnings.push(format!("{what} dropped: {e}"));
            Ok(None)
        }
        Err(e) => Err(EngineError::compile(what, e)),
    }
}

/// The expression language a compat mode runs (`CompatRules::md3_ns_eel2`): the dialect the engine's
/// [`GlobalMemory`] owner is created with, which every context built from it follows.
pub fn eel_dialect(rules: &CompatRules) -> Dialect {
    if rules.md3_ns_eel2 {
        Dialect::NsEel2
    } else {
        Dialect::ProjectMEval
    }
}

fn set(ctx: &mut Context, v: lactea_eel::Var, x: f64) {
    ctx.set(v, x);
}

// ---------------------------------------------------------------------------------------------
// preset per-frame
// ---------------------------------------------------------------------------------------------

pub struct PerFrame {
    pub ctx: Context,
    pub v: PfVars,
    pub prog: Option<Program>,
    pub q_after_init: [f64; MAX_Q],
}

impl PerFrame {
    pub fn new(gm: &Arc<GlobalMemory>) -> Self {
        let mut ctx = Context::new(gm.clone());
        ctx.reset_vars();
        let v = PfVars::register(&mut ctx);
        Self {
            ctx,
            v,
            prog: None,
            q_after_init: [0.0; MAX_Q],
        }
    }

    /// MilkDrop 3: `q33..q64` join the context's variables (`monitor2` / `monitor3` are plain
    /// user variables and persist like `monitor`). The `mouse.*` names are not registered: MD3's expression
    /// grammar has no dotted identifiers, so preset code cannot read them (the mouse is a shader input only).
    pub fn extend_md3(&mut self) {
        crate::vars::extend_md3(&mut self.ctx, &mut self.v.q, &mut self.v.t);
    }

    pub fn q(&self, i: usize) -> f64 {
        self.ctx.get(self.v.q[i])
    }

    /// `PerFrameContext::LoadStateVariables`.
    pub fn load_state(&mut self, b: &BaseValues, e: &Env) {
        let c = &mut self.ctx;
        let v = &self.v;
        let f = |x: f32| f64::from(x);
        set(c, v.zoom, f(b.zoom));
        set(c, v.zoomexp, f(b.zoom_exponent));
        set(c, v.rot, f(b.rot));
        set(c, v.warp, f(b.warp));
        set(c, v.cx, f(b.cx));
        set(c, v.cy, f(b.cy));
        set(c, v.dx, f(b.dx));
        set(c, v.dy, f(b.dy));
        set(c, v.sx, f(b.sx));
        set(c, v.sy, f(b.sy));
        set(c, v.time, f(e.time));
        set(c, v.fps, f(e.fps));
        set(c, v.bass, f(e.bass));
        set(c, v.mid, f(e.mid));
        set(c, v.treb, f(e.treb));
        set(c, v.bass_att, f(e.bass_att));
        set(c, v.mid_att, f(e.mid_att));
        set(c, v.treb_att, f(e.treb_att));
        set(c, v.frame, f64::from(e.frame));
        for (q, &var) in self.q_after_init.iter().zip(&v.q) {
            c.set(var, *q);
        }
        set(c, v.progress, f(e.progress));
        set(c, v.decay, f(b.decay));
        set(c, v.wave_a, f(b.wave_alpha));
        set(c, v.wave_r, f(b.wave_r));
        set(c, v.wave_g, f(b.wave_g));
        set(c, v.wave_b, f(b.wave_b));
        set(c, v.wave_x, f(b.wave_x));
        set(c, v.wave_y, f(b.wave_y));
        set(c, v.wave_mystery, f(b.wave_param));
        set(c, v.wave_mode, f64::from(b.wave_mode));
        set(c, v.ob_size, f(b.ob_size));
        set(c, v.ob_r, f(b.ob_r));
        set(c, v.ob_g, f(b.ob_g));
        set(c, v.ob_b, f(b.ob_b));
        set(c, v.ob_a, f(b.ob_a));
        set(c, v.ib_size, f(b.ib_size));
        set(c, v.ib_r, f(b.ib_r));
        set(c, v.ib_g, f(b.ib_g));
        set(c, v.ib_b, f(b.ib_b));
        set(c, v.ib_a, f(b.ib_a));
        set(c, v.mv_x, f(b.mv_x));
        set(c, v.mv_y, f(b.mv_y));
        set(c, v.mv_dx, f(b.mv_dx));
        set(c, v.mv_dy, f(b.mv_dy));
        set(c, v.mv_l, f(b.mv_l));
        set(c, v.mv_r, f(b.mv_r));
        set(c, v.mv_g, f(b.mv_g));
        set(c, v.mv_b, f(b.mv_b));
        set(c, v.mv_a, f(b.mv_a));
        set(c, v.echo_zoom, f(b.video_echo_zoom));
        set(c, v.echo_alpha, f(b.video_echo_alpha));
        set(c, v.echo_orient, f64::from(b.video_echo_orientation));
        set(c, v.wave_usedots, f64::from(u8::from(b.wave_dots)));
        set(c, v.wave_thick, f64::from(u8::from(b.wave_thick)));
        set(c, v.wave_additive, f64::from(u8::from(b.additive_waves)));
        set(
            c,
            v.wave_brighten,
            f64::from(u8::from(b.maximize_wave_color)),
        );
        set(c, v.darken_center, f64::from(u8::from(b.darken_center)));
        set(c, v.gamma, f(b.gamma_adj));
        set(c, v.wrap, f64::from(u8::from(b.tex_wrap)));
        set(c, v.invert, f64::from(u8::from(b.invert)));
        set(c, v.brighten, f64::from(u8::from(b.brighten)));
        set(c, v.darken, f64::from(u8::from(b.darken)));
        set(c, v.solarize, f64::from(u8::from(b.solarize)));
        set(c, v.meshx, f64::from(e.mesh[0]));
        set(c, v.meshy, f64::from(e.mesh[1]));
        set(c, v.pixelsx, f64::from(e.pixels[0]));
        set(c, v.pixelsy, f64::from(e.pixels[1]));
        set(c, v.aspectx, f(e.inv_aspect[0]));
        set(c, v.aspecty, f(e.inv_aspect[1]));
        set(c, v.blur1_min, f(b.blur1_min));
        set(c, v.blur2_min, f(b.blur2_min));
        set(c, v.blur3_min, f(b.blur3_min));
        set(c, v.blur1_max, f(b.blur1_max));
        set(c, v.blur2_max, f(b.blur2_max));
        set(c, v.blur3_max, f(b.blur3_max));
        set(c, v.blur1_edge_darken, f(b.blur1_edge_darken));
    }

    pub fn run(&mut self) {
        if let Some(p) = &self.prog {
            self.ctx.run(p);
        }
    }

    /// projectM clamps `gamma` to 0..8 and `echo_zoom` to 0.001..1000 after the equations.
    pub fn clamp(&mut self) {
        let g = self.ctx.get(self.v.gamma).clamp(0.0, 8.0);
        self.ctx.set(self.v.gamma, g);
        let ez = self.ctx.get(self.v.echo_zoom).clamp(0.001, 1000.0);
        self.ctx.set(self.v.echo_zoom, ez);
    }
}

// ---------------------------------------------------------------------------------------------
// per-vertex ("per-pixel")
// ---------------------------------------------------------------------------------------------

pub struct PerPixel {
    pub ctx: Context,
    pub v: PpVars,
    pub prog: Option<Program>,
}

impl PerPixel {
    pub fn new(gm: &Arc<GlobalMemory>) -> Self {
        let mut ctx = Context::new(gm.clone());
        ctx.reset_vars();
        let v = PpVars::register(&mut ctx);
        Self {
            ctx,
            v,
            prog: None,
        }
    }

    /// MilkDrop 3: `q33..q64` join the context's variables.
    pub fn extend_md3(&mut self) {
        crate::vars::extend_md3(&mut self.ctx, &mut self.v.q, &mut self.v.t);
    }

    /// `PerPixelContext::LoadStateReadOnlyVariables` (note: `aspectx/y` are the *non*-inverse
    /// aspect here, unlike the per-frame context).
    pub fn load_readonly(&mut self, pf: &PerFrame, e: &Env) {
        let c = &mut self.ctx;
        let v = &self.v;
        let g = |x| pf.ctx.get(x);
        set(c, v.time, g(pf.v.time));
        set(c, v.fps, g(pf.v.fps));
        set(c, v.frame, g(pf.v.frame));
        set(c, v.progress, g(pf.v.progress));
        set(c, v.bass, g(pf.v.bass));
        set(c, v.mid, g(pf.v.mid));
        set(c, v.treb, g(pf.v.treb));
        set(c, v.bass_att, g(pf.v.bass_att));
        set(c, v.mid_att, g(pf.v.mid_att));
        set(c, v.treb_att, g(pf.v.treb_att));
        set(c, v.meshx, f64::from(e.mesh[0]));
        set(c, v.meshy, f64::from(e.mesh[1]));
        set(c, v.pixelsx, f64::from(e.pixels[0]));
        set(c, v.pixelsy, f64::from(e.pixels[1]));
        set(c, v.aspectx, f64::from(e.aspect[0]));
        set(c, v.aspecty, f64::from(e.aspect[1]));
    }

    /// `PerPixelContext::LoadPerFrameQVariables` (once per frame, not per vertex).
    pub fn load_q(&mut self, pf: &PerFrame) {
        for (i, &var) in self.v.q.iter().enumerate() {
            self.ctx.set(var, pf.q(i));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// custom wave
// ---------------------------------------------------------------------------------------------

pub struct WaveRt {
    pub cfg: CustomWave,
    pub pf: Context,
    pub pfv: WfVars,
    pub pf_prog: Option<Program>,
    pub pp: Context,
    pub ppv: WpVars,
    pub pp_prog: Option<Program>,
    pub t_after_init: [f64; MAX_T],
}

impl WaveRt {
    pub fn new(gm: &Arc<GlobalMemory>, cfg: CustomWave) -> Self {
        let mut pf = Context::new(gm.clone());
        pf.reset_vars();
        let pfv = WfVars::register(&mut pf);
        let mut pp = Context::new(gm.clone());
        pp.reset_vars();
        let ppv = WpVars::register(&mut pp);
        Self {
            cfg,
            pf,
            pfv,
            pf_prog: None,
            pp,
            ppv,
            pp_prog: None,
            t_after_init: [0.0; MAX_T],
        }
    }

    /// MilkDrop 3: `q33..q64` and `t9..t16` join both contexts' variables.
    pub fn extend_md3(&mut self) {
        crate::vars::extend_md3(&mut self.pf, &mut self.pfv.q, &mut self.pfv.t);
        crate::vars::extend_md3(&mut self.pp, &mut self.ppv.q, &mut self.ppv.t);
    }

    pub fn enabled(&self) -> bool {
        self.cfg.enabled != 0
    }

    /// `WaveformPerFrameContext::LoadStateVariables`.
    pub fn load_state(&mut self, parent: &PerFrame, e: &Env) {
        let c = &mut self.pf;
        let v = &self.pfv;
        let f = |x: f32| f64::from(x);
        set(c, v.time, f(e.time));
        set(c, v.frame, f64::from(e.frame));
        set(c, v.fps, f(e.fps));
        set(c, v.progress, f(e.progress));
        set(c, v.bass, f(e.bass));
        set(c, v.mid, f(e.mid));
        set(c, v.treb, f(e.treb));
        set(c, v.bass_att, f(e.bass_att));
        set(c, v.mid_att, f(e.mid_att));
        set(c, v.treb_att, f(e.treb_att));
        for (i, &var) in v.q.iter().enumerate() {
            c.set(var, parent.q(i));
        }
        for (&var, &t) in v.t.iter().zip(self.t_after_init.iter()) {
            c.set(var, t);
        }
        set(c, v.r, f(self.cfg.r));
        set(c, v.g, f(self.cfg.g));
        set(c, v.b, f(self.cfg.b));
        set(c, v.a, f(self.cfg.a));
        set(c, v.samples, f64::from(self.cfg.samples));
    }

    /// `WaveformPerPointContext::LoadReadOnlyStateVariables` (copied from the preset's per-frame context).
    pub fn load_point_readonly(&mut self, parent: &PerFrame) {
        let c = &mut self.pp;
        let v = &self.ppv;
        let g = |x| parent.ctx.get(x);
        set(c, v.time, g(parent.v.time));
        set(c, v.frame, g(parent.v.frame));
        set(c, v.fps, g(parent.v.fps));
        set(c, v.progress, g(parent.v.progress));
        set(c, v.bass, g(parent.v.bass));
        set(c, v.mid, g(parent.v.mid));
        set(c, v.treb, g(parent.v.treb));
        set(c, v.bass_att, g(parent.v.bass_att));
        set(c, v.mid_att, g(parent.v.mid_att));
        set(c, v.treb_att, g(parent.v.treb_att));
    }

    /// `InitPerPointEvaluationVariables`: q/t of the per-frame context go to the per-point one.
    pub fn init_point_qt(&mut self) {
        for (&src, &dst) in self.pfv.q.iter().zip(&self.ppv.q) {
            let x = self.pf.get(src);
            self.pp.set(dst, x);
        }
        for (&src, &dst) in self.pfv.t.iter().zip(&self.ppv.t) {
            let x = self.pf.get(src);
            self.pp.set(dst, x);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// custom shape
// ---------------------------------------------------------------------------------------------

pub struct ShapeRt {
    pub cfg: CustomShape,
    pub ctx: Context,
    pub v: ShVars,
    pub prog: Option<Program>,
    pub t_after_init: [f64; MAX_T],
}

impl ShapeRt {
    pub fn new(gm: &Arc<GlobalMemory>, cfg: CustomShape) -> Self {
        let mut ctx = Context::new(gm.clone());
        ctx.reset_vars();
        let v = ShVars::register(&mut ctx);
        Self {
            cfg,
            ctx,
            v,
            prog: None,
            t_after_init: [0.0; MAX_T],
        }
    }

    /// MilkDrop 3: `q33..q64` and `t9..t16` join the context's variables.
    pub fn extend_md3(&mut self) {
        crate::vars::extend_md3(&mut self.ctx, &mut self.v.q, &mut self.v.t);
    }

    pub fn enabled(&self) -> bool {
        self.cfg.enabled != 0
    }

    /// `ShapePerFrameContext::LoadStateVariables`; `frame_q` is `PresetState::frameQVariables`.
    pub fn load_state(&mut self, frame_q: &[f64; MAX_Q], e: &Env, inst: i32) {
        let c = &mut self.ctx;
        let v = &self.v;
        let s = &self.cfg;
        let f = |x: f32| f64::from(x);
        set(c, v.time, f(e.time));
        set(c, v.frame, f64::from(e.frame));
        set(c, v.fps, f(e.fps));
        set(c, v.progress, f(e.progress));
        set(c, v.bass, f(e.bass));
        set(c, v.mid, f(e.mid));
        set(c, v.treb, f(e.treb));
        set(c, v.bass_att, f(e.bass_att));
        set(c, v.mid_att, f(e.mid_att));
        set(c, v.treb_att, f(e.treb_att));
        for (&var, &q) in v.q.iter().zip(frame_q.iter()) {
            c.set(var, q);
        }
        for (&var, &t) in v.t.iter().zip(self.t_after_init.iter()) {
            c.set(var, t);
        }
        set(c, v.x, f(s.x));
        set(c, v.y, f(s.y));
        set(c, v.rad, f(s.rad));
        set(c, v.ang, f(s.ang));
        set(c, v.tex_zoom, f(s.tex_zoom));
        set(c, v.tex_ang, f(s.tex_ang));
        set(c, v.sides, f64::from(s.sides));
        set(c, v.additive, f64::from(s.additive));
        set(c, v.textured, f64::from(s.textured));
        set(c, v.num_inst, f64::from(s.instances));
        set(c, v.instance, f64::from(inst));
        set(c, v.thick, f64::from(s.thick_outline));
        set(c, v.r, f(s.r));
        set(c, v.g, f(s.g));
        set(c, v.b, f(s.b));
        set(c, v.a, f(s.a));
        set(c, v.r2, f(s.r2));
        set(c, v.g2, f(s.g2));
        set(c, v.b2, f(s.b2));
        set(c, v.a2, f(s.a2));
        set(c, v.border_r, f(s.border_r));
        set(c, v.border_g, f(s.border_g));
        set(c, v.border_b, f(s.border_b));
        set(c, v.border_a, f(s.border_a));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lactea_preset::Preset;
    use lactea_render::Compat;

    /// A statement split inside an identifier (`my_` / `var`), as MD2 presets written in the editor have it, and a
    /// trailing comment: MD2 deletes the line separators without a replacement.
    const SPLIT: &str = "per_frame_1=my_var = 5;\n\
                         per_frame_2=q1 = my_\n\
                         per_frame_3=var*2; // doubled\n\
                         per_frame_4=q2 = q1 + 1;\n";

    fn run(preset: &Preset, compat: Compat) -> Result<(f64, f64), CompileError> {
        let gm = Arc::new(GlobalMemory::new());
        let mut pf = PerFrame::new(&gm);
        pf.prog = compile_opt(&mut pf.ctx, &preset.per_frame, compat.rules().md2_code_join)?;
        pf.run();
        Ok((pf.q(0), pf.q(1)))
    }

    #[test]
    fn split_identifier_compiles_under_milkdrop_code_join_only() {
        let preset = Preset::parse(SPLIT.as_bytes()).unwrap();
        assert_eq!(run(&preset, Compat::MilkDrop2).unwrap(), (10.0, 11.0));
        assert_eq!(run(&preset, Compat::MilkDrop3).unwrap(), (10.0, 11.0));
        // projectM joins with `\n`: `my_` and `var` stay two tokens
        assert!(run(&preset, Compat::ProjectM).is_err());
    }

    #[test]
    fn blank_or_comment_only_code_is_no_code_under_milkdrop_join() {
        let gm = Arc::new(GlobalMemory::new());
        let mut ctx = Context::new(gm);
        for src in ["", "\u{1}", "  \u{1} \u{1}", "// only a comment\u{1}"] {
            assert!(
                compile_opt(&mut ctx, src, true).unwrap().is_none(),
                "{src:?}"
            );
        }
        // projectM keeps a comment-only block as an empty program
        assert!(compile_opt(&mut ctx, "// c\u{1}", false).unwrap().is_some());
    }

    /// The data flow of Annex A 4.7: q1..q64 restart from the init snapshot every frame, go pf -> pv, and the
    /// custom waves / shapes see the *current* preset q (64) and their own t (16).
    #[test]
    fn md3_q64_t16_data_flow() {
        let gm = Arc::new(GlobalMemory::new());
        let (mut pf, mut pp) = (PerFrame::new(&gm), PerPixel::new(&gm));
        pf.extend_md3();
        pp.extend_md3();
        assert_eq!((pf.v.q.len(), pp.v.q.len()), (64, 64));
        let mut wave = WaveRt::new(&gm, CustomWave::default());
        wave.extend_md3();
        let mut shape = ShapeRt::new(&gm, CustomShape::default());
        shape.extend_md3();
        assert_eq!((wave.pfv.t.len(), wave.ppv.q.len()), (16, 64));
        assert_eq!((shape.v.q.len(), shape.v.t.len()), (64, 16));

        // init code: q64 and q33 survive as the post-init snapshot
        pf.prog = compile_opt(&mut pf.ctx, "q64 = 7; q33 = 3;", false).unwrap();
        pf.run();
        for i in 0..64 {
            pf.q_after_init[i] = pf.q(i);
        }
        // frame: per-frame code changes q64 and writes q40; the next frame starts from the snapshot again
        pf.prog = compile_opt(&mut pf.ctx, "q40 = q64 + 1; q64 = 99;", false).unwrap();
        let env = Env::default();
        for _ in 0..2 {
            pf.load_state(&BaseValues::default(), &env);
            pp.load_readonly(&pf, &env);
            assert_eq!(pf.q(63), 7.0, "q64 restarts from the init value");
            pf.run();
            assert_eq!(pf.q(39), 8.0);
        }
        pp.load_q(&pf);
        assert_eq!(
            (pp.ctx.get(pp.v.q[39]), pp.ctx.get(pp.v.q[63])),
            (8.0, 99.0)
        );
        // waves and shapes receive the current preset q, all 64
        wave.load_state(&pf, &env);
        assert_eq!(wave.pf.get(wave.pfv.q[39]), 8.0);
        assert_eq!(wave.pf.get(wave.pfv.q[63]), 99.0);
        let mut frame_q = [0.0; MAX_Q];
        for (i, q) in frame_q.iter_mut().enumerate() {
            *q = pf.q(i);
        }
        shape.t_after_init[15] = 0.5;
        shape.load_state(&frame_q, &env, 0);
        assert_eq!(shape.ctx.get(shape.v.q[63]), 99.0);
        assert_eq!(shape.ctx.get(shape.v.t[15]), 0.5);
        // the wave's per-frame q/t go to its per-point context
        wave.pf.set(wave.pfv.t[12], 4.0);
        wave.pf.set(wave.pfv.q[50], 5.0);
        wave.init_point_qt();
        assert_eq!(wave.pp.get(wave.ppv.t[12]), 4.0);
        assert_eq!(wave.pp.get(wave.ppv.q[50]), 5.0);
    }

    #[test]
    fn projectm_contexts_keep_32_q_and_8_t() {
        let gm = Arc::new(GlobalMemory::new());
        let (pf, wave) = (PerFrame::new(&gm), WaveRt::new(&gm, CustomWave::default()));
        assert_eq!((pf.v.q.len(), wave.pfv.t.len()), (32, 8));
        assert!(pf.ctx.find_var("q33").is_none());
    }

    fn gm(compat: Compat) -> Arc<GlobalMemory> {
        Arc::new(GlobalMemory::with_dialect(eel_dialect(&compat.rules())))
    }

    fn part(compat: Compat, src: &str) -> (Result<Option<Program>, EngineError>, Vec<String>) {
        let mut ctx = Context::new(gm(compat));
        let mut warnings = Vec::new();
        let r = compile_part(
            &mut ctx,
            src,
            &compat.rules(),
            "per-frame code",
            &mut warnings,
        );
        (r, warnings)
    }

    #[test]
    fn only_milkdrop3_runs_ns_eel2() {
        assert_eq!(
            eel_dialect(&Compat::ProjectM.rules()),
            Dialect::ProjectMEval
        );
        assert_eq!(
            eel_dialect(&Compat::MilkDrop2.rules()),
            Dialect::ProjectMEval
        );
        assert_eq!(eel_dialect(&Compat::MilkDrop3.rules()), Dialect::NsEel2);
    }

    /// MD3 3.37 rejects a '.' inside identifiers (measured on the real program), so a part that reads `mouse.x`
    /// as source text is dropped: preset code cannot read the mouse, only shaders can.
    #[test]
    fn dotted_names_do_not_parse_under_milkdrop3() {
        let (r, warnings) = part(Compat::MilkDrop3, "q1 = mouse.x * 2;\u{1}");
        assert!(r.unwrap().is_none());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    /// ns-eel2 numerics through the preset contexts: the unguarded `/` is filtered by `=`, `%` is integer.
    #[test]
    fn per_frame_code_follows_the_ns_eel2_numerics_under_milkdrop3() {
        let preset =
            Preset::parse(b"per_frame_1=q1 = 1/0; a = -7; q2 = a % 3; q3 = int(-2.5);\n").unwrap();
        let run = |compat: Compat| {
            let mut pf = PerFrame::new(&gm(compat));
            pf.prog =
                compile_opt(&mut pf.ctx, &preset.per_frame, compat.rules().md2_code_join).unwrap();
            pf.run();
            (pf.q(0), pf.q(1), pf.q(2))
        };
        assert_eq!(run(Compat::MilkDrop3), (0.0, 1.0, -3.0));
        assert_eq!(run(Compat::MilkDrop2), (0.0, -1.0, -3.0));
    }

    #[test]
    fn a_failing_part_is_dropped_under_milkdrop3_and_rejects_the_preset_otherwise() {
        let bad = "q1 = (;\u{1}";
        for compat in [Compat::ProjectM, Compat::MilkDrop2] {
            let (r, warnings) = part(compat, bad);
            assert!(matches!(r, Err(EngineError::Compile { .. })), "{compat:?}");
            assert!(warnings.is_empty());
        }
        let (r, warnings) = part(Compat::MilkDrop3, bad);
        assert!(r.unwrap().is_none());
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].starts_with("per-frame code dropped: "),
            "{warnings:?}"
        );
        // a good part is untouched
        let (r, warnings) = part(Compat::MilkDrop3, "q1 = 1;\u{1}");
        assert!(r.unwrap().is_some() && warnings.is_empty());
    }

    /// The whole part goes, not just the failing statement.
    #[test]
    fn the_good_statements_of_a_failing_part_do_not_run() {
        let preset = Preset::parse(b"per_frame_1=q1 = 5;\nper_frame_2=q2 = (;\n").unwrap();
        let compat = Compat::MilkDrop3;
        let mut pf = PerFrame::new(&gm(compat));
        let mut warnings = Vec::new();
        pf.prog = compile_part(
            &mut pf.ctx,
            &preset.per_frame,
            &compat.rules(),
            "per-frame code",
            &mut warnings,
        )
        .unwrap();
        pf.run();
        assert!(pf.prog.is_none());
        assert_eq!((pf.q(0), pf.q(1)), (0.0, 0.0));
    }
}
