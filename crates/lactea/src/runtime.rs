//! Expression contexts of a loaded preset and how projectM feeds them.
//!
//! Behaviour follows projectM: `PerFrameContext`, `PerPixelContext`,
//! `WaveformPerFrameContext` / `WaveformPerPointContext`, `ShapePerFrameContext` and
//! `MilkdropPreset::{PerFrameUpdate, CompileCodeAndRunInitExpressions}`.

use crate::vars::{PfVars, PpVars, ShVars, WfVars, WpVars};
use lactea_eel::{CompileError, Context, GlobalMemory, Program};
use lactea_preset::{BaseValues, CustomShape, CustomWave, code_to_source};
use std::sync::Arc;

pub const NUM_Q: usize = 32;
pub const NUM_T: usize = 8;

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

/// Compile `src` into `ctx`; empty source gives `None` (projectM leaves the handle null).
/// A non-empty source that compiles to an empty program (comments only) is kept as `Some`.
pub fn compile_opt(ctx: &mut Context, src: &str) -> Result<Option<Program>, CompileError> {
    let src = code_to_source(src);
    if src.is_empty() {
        return Ok(None);
    }
    ctx.compile(&src).map(Some)
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
    pub q_after_init: [f64; NUM_Q],
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
            q_after_init: [0.0; NUM_Q],
        }
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
        for (i, q) in self.q_after_init.iter().enumerate() {
            c.set(v.q[i], *q);
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
        Self { ctx, v, prog: None }
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
        for i in 0..NUM_Q {
            self.ctx.set(self.v.q[i], pf.q(i));
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
    pub t_after_init: [f64; NUM_T],
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
            t_after_init: [0.0; NUM_T],
        }
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
        for i in 0..NUM_Q {
            c.set(v.q[i], parent.q(i));
        }
        for i in 0..NUM_T {
            c.set(v.t[i], self.t_after_init[i]);
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
        for i in 0..NUM_Q {
            let x = self.pf.get(self.pfv.q[i]);
            self.pp.set(self.ppv.q[i], x);
        }
        for i in 0..NUM_T {
            let x = self.pf.get(self.pfv.t[i]);
            self.pp.set(self.ppv.t[i], x);
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
    pub t_after_init: [f64; NUM_T],
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
            t_after_init: [0.0; NUM_T],
        }
    }

    pub fn enabled(&self) -> bool {
        self.cfg.enabled != 0
    }

    /// `ShapePerFrameContext::LoadStateVariables`; `frame_q` is `PresetState::frameQVariables`.
    pub fn load_state(&mut self, frame_q: &[f64; NUM_Q], e: &Env, inst: i32) {
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
        for (&var, &q) in v.q.iter().zip(frame_q.iter()).take(NUM_Q) {
            c.set(var, q);
        }
        for (&var, &t) in v.t.iter().zip(self.t_after_init.iter()).take(NUM_T) {
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
