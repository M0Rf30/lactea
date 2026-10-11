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
}
