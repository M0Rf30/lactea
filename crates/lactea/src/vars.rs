//! Variable tables of the five expression contexts (projectM `PerFrameContext`,
//! `PerPixelContext`, `WaveformPerFrameContext`, `WaveformPerPointContext`,
//! `ShapePerFrameContext`): names resolved once to [`Var`] handles.

use lactea_eel::{Context, Var};

macro_rules! var_struct {
    ($name:ident, q = $q:literal, t = $t:literal, [$($f:ident),* $(,)?]) => {
        // Every listed field is registered in the context (its slot exists for presets and dumps)
        // even when the engine never reads it back through the handle.
        #[allow(dead_code)]
        pub struct $name {
            $(pub $f: Var,)*
            /// `q1..q32` (empty when this context has none).
            pub q: Vec<Var>,
            /// `t1..t8` (empty when this context has none).
            pub t: Vec<Var>,
        }

        impl $name {
            pub fn register(ctx: &mut Context) -> Self {
                $(let $f = ctx.var(stringify!($f));)*
                let q = if $q { (1..=32).map(|i| ctx.var(&format!("q{i}"))).collect() } else { Vec::new() };
                let t = if $t { (1..=8).map(|i| ctx.var(&format!("t{i}"))).collect() } else { Vec::new() };
                Self { $($f,)* q, t }
            }
        }
    };
}

var_struct!(
    PfVars,
    q = true,
    t = false,
    [
        zoom,
        zoomexp,
        rot,
        warp,
        cx,
        cy,
        dx,
        dy,
        sx,
        sy,
        time,
        fps,
        bass,
        mid,
        treb,
        bass_att,
        mid_att,
        treb_att,
        frame,
        decay,
        wave_a,
        wave_r,
        wave_g,
        wave_b,
        wave_x,
        wave_y,
        wave_mystery,
        wave_mode,
        progress,
        ob_size,
        ob_r,
        ob_g,
        ob_b,
        ob_a,
        ib_size,
        ib_r,
        ib_g,
        ib_b,
        ib_a,
        mv_x,
        mv_y,
        mv_dx,
        mv_dy,
        mv_l,
        mv_r,
        mv_g,
        mv_b,
        mv_a,
        echo_zoom,
        echo_alpha,
        echo_orient,
        wave_usedots,
        wave_thick,
        wave_additive,
        wave_brighten,
        darken_center,
        gamma,
        wrap,
        invert,
        brighten,
        darken,
        solarize,
        meshx,
        meshy,
        pixelsx,
        pixelsy,
        aspectx,
        aspecty,
        blur1_min,
        blur2_min,
        blur3_min,
        blur1_max,
        blur2_max,
        blur3_max,
        blur1_edge_darken
    ]
);

var_struct!(
    PpVars,
    q = true,
    t = false,
    [
        zoom, zoomexp, rot, warp, cx, cy, dx, dy, sx, sy, time, fps, bass, mid, treb, bass_att,
        mid_att, treb_att, frame, x, y, rad, ang, progress, meshx, meshy, pixelsx, pixelsy,
        aspectx, aspecty
    ]
);

var_struct!(
    WfVars,
    q = true,
    t = true,
    [
        time, fps, frame, progress, bass, mid, treb, bass_att, mid_att, treb_att, r, g, b, a,
        samples
    ]
);

var_struct!(
    WpVars,
    q = true,
    t = true,
    [
        time, fps, frame, progress, bass, mid, treb, bass_att, mid_att, treb_att, sample, value1,
        value2, x, y, r, g, b, a
    ]
);

var_struct!(
    ShVars,
    q = true,
    t = true,
    [
        time, fps, frame, progress, bass, mid, treb, bass_att, mid_att, treb_att, x, y, rad, ang,
        tex_ang, tex_zoom, sides, textured, instance, num_inst, additive, thick, r, g, b, a, r2,
        g2, b2, a2, border_r, border_g, border_b, border_a
    ]
);

/// MilkDrop 3: append `q33..q64` to `q` and, for contexts that have `t` variables, `t9..t16` to `t`.
pub fn extend_md3(ctx: &mut Context, q: &mut Vec<Var>, t: &mut Vec<Var>) {
    q.extend((q.len() + 1..=lactea_abi::NUM_Q_MD3).map(|i| ctx.var(&format!("q{i}"))));
    if !t.is_empty() {
        t.extend((t.len() + 1..=lactea_abi::NUM_T_MD3).map(|i| ctx.var(&format!("t{i}"))));
    }
}

/// BeatDrop's host variables (`CompatRules::md2_input_vars`): `mousex mousey mousedown mouseclick`,
/// `ctrllt ctrlrt ctrldn ctrlup` and, in the per-frame context only, `year month day weekday hour minute second
/// milliseconds totalseconds` (`state.cpp:349-365, 402-409`).
pub struct HostVars {
    pub mouse: [Var; 4],
    pub ctrl: [Var; 4],
    /// Empty in the per-vertex context.
    pub date: Vec<Var>,
}

/// Register [`HostVars`] in `ctx`.
pub fn register_host(ctx: &mut Context, with_date: bool) -> HostVars {
    let mut reg = |names: [&str; 4]| names.map(|n| ctx.var(n));
    let mouse = reg(["mousex", "mousey", "mousedown", "mouseclick"]);
    let ctrl = reg(["ctrllt", "ctrlrt", "ctrldn", "ctrlup"]);
    let date = if with_date {
        [
            "year",
            "month",
            "day",
            "weekday",
            "hour",
            "minute",
            "second",
            "milliseconds",
            "totalseconds",
        ]
        .iter()
        .map(|n| ctx.var(n))
        .collect()
    } else {
        Vec::new()
    };
    HostVars { mouse, ctrl, date }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lactea_eel::GlobalMemory;
    use std::sync::Arc;

    #[test]
    fn registers_all_names() {
        let mut ctx = Context::new(Arc::new(GlobalMemory::new()));
        let v = PfVars::register(&mut ctx);
        assert_eq!(v.q.len(), 32);
        assert!(ctx.find_var("blur1_edge_darken").is_some());
        assert!(ctx.find_var("Q32").is_some());
        // 75 scalar builtins + 32 q
        assert_eq!(ctx.var_count(), 75 + 32);
        let mut ctx2 = Context::new(Arc::new(GlobalMemory::new()));
        let s = ShVars::register(&mut ctx2);
        assert_eq!((s.q.len(), s.t.len()), (32, 8));
    }

    #[test]
    fn md3_extension_adds_q33_to_q64_and_t9_to_t16() {
        let mut ctx = Context::new(Arc::new(GlobalMemory::new()));
        let mut v = PfVars::register(&mut ctx);
        extend_md3(&mut ctx, &mut v.q, &mut v.t);
        // per-frame contexts have no `t`: it stays empty
        assert_eq!((v.q.len(), v.t.len()), (64, 0));
        assert_eq!(ctx.find_var("q64"), Some(v.q[63]));
        assert_eq!(ctx.var_count(), 75 + 64);
        let mut ctx2 = Context::new(Arc::new(GlobalMemory::new()));
        let mut s = ShVars::register(&mut ctx2);
        extend_md3(&mut ctx2, &mut s.q, &mut s.t);
        assert_eq!((s.q.len(), s.t.len()), (64, 16));
        assert_eq!(ctx2.find_var("t16"), Some(s.t[15]));
    }
}
