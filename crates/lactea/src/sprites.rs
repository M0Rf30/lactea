//! MilkDrop 3 preset sprites: per-frame state, geometry and loading (MD3 3.37, docs/md3.md "Sprites").
//!
//! A preset carries at most one sprite: an image reference, placement numbers (`SpriteX` ..) and optional
//! per-frame code. [`SpriteRt`] owns its expression context and, every frame, turns the code's outputs into the
//! [`SpriteDraw`] the renderer draws. Nothing here touches the GPU except [`load`], which registers the image
//! with the renderer.

use crate::PresetReport;
use crate::cache::MediaCache;
use crate::media::Media;
use crate::runtime::compile_part;
use crate::textures::TextureProvider;
use lactea_eel::{Context, GlobalMemory, Program, Var};
use lactea_preset::PresetSprite;
use lactea_render::{CompatRules, Renderer, SpriteDraw, SpriteVertex, gif_frame_index};
use std::sync::Arc;

/// What the sprite's code and motion see of the host this frame.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SpriteInputs {
    /// `GetTime()`: the absolute time of the frame in seconds (the sprite's `time` counts from its start).
    pub time: f64,
    /// `GetFrame()`.
    pub frame: u32,
    /// `GetFps()`.
    pub fps: f32,
    /// Blend progress of the preset (`progress`).
    pub progress: f32,
    pub bass: f32,
    pub mid: f32,
    pub treb: f32,
    pub bass_att: f32,
    pub mid_att: f32,
    pub treb_att: f32,
}

/// Registered variables of a sprite's expression context (`time` .. `treb_att` are inputs, the rest outputs).
struct Vars {
    time: Var,
    frame: Var,
    fps: Var,
    progress: Var,
    bass: Var,
    bass_att: Var,
    mid: Var,
    mid_att: Var,
    treb: Var,
    treb_att: Var,
    x: Var,
    y: Var,
    sx: Var,
    sy: Var,
    rot: Var,
    flipx: Var,
    flipy: Var,
    r: Var,
    g: Var,
    b: Var,
    a: Var,
    repeatx: Var,
    repeaty: Var,
    done: Var,
    burn: Var,
}

impl Vars {
    fn register(ctx: &mut Context) -> Self {
        // only registered: the preset's `SpriteBlend` decides the mode, but code may still assign `blendmode`
        let _ = ctx.var("blendmode");
        Self {
            time: ctx.var("time"),
            frame: ctx.var("frame"),
            fps: ctx.var("fps"),
            progress: ctx.var("progress"),
            bass: ctx.var("bass"),
            bass_att: ctx.var("bass_att"),
            mid: ctx.var("mid"),
            mid_att: ctx.var("mid_att"),
            treb: ctx.var("treb"),
            treb_att: ctx.var("treb_att"),
            x: ctx.var("x"),
            y: ctx.var("y"),
            sx: ctx.var("sx"),
            sy: ctx.var("sy"),
            rot: ctx.var("rot"),
            flipx: ctx.var("flipx"),
            flipy: ctx.var("flipy"),
            r: ctx.var("r"),
            g: ctx.var("g"),
            b: ctx.var("b"),
            a: ctx.var("a"),
            repeatx: ctx.var("repeatx"),
            repeaty: ctx.var("repeaty"),
            done: ctx.var("done"),
            burn: ctx.var("burn"),
        }
    }
}

/// `min(hi, max(lo, v))` with C's `fminf` / `fmaxf` NaN behaviour (a NaN becomes `lo`).
fn clampf(v: f32, lo: f32, hi: f32) -> f32 {
    hi.min(lo.max(v))
}

/// Placement of one sprite quad, after the clamps and the preset's additive offsets.
#[derive(Clone, Copy, Debug)]
pub(crate) struct QuadParams {
    /// Centre in clip units (`var_x * 2 - 1 + SpriteX`).
    pub x: f32,
    /// Centre y (`var_y * 2 - 1 + SpriteY`), y down.
    pub y: f32,
    /// Half width before the aspect corrections.
    pub sx: f32,
    /// Half height before the aspect corrections.
    pub sy: f32,
    /// Rotation in radians (positive turns clockwise on screen).
    pub rot: f32,
    pub flip_x: bool,
    pub flip_y: bool,
    /// UV tiling.
    pub repeat_x: f32,
    pub repeat_y: f32,
    /// Pixel size of the image file (frame 0 for a GIF).
    pub image: (u32, u32),
    /// Pixel size of the render target.
    pub screen: (u32, u32),
    /// The last frame of a burning sprite (`done && burn`): MD3 additionally corrects for a 4:3 target.
    pub final_burn: bool,
}

/// The quad of a sprite in MD3's sprite space (`±1` = target edge, y down), triangle-strip order.
///
/// Transform chain: image aspect fix, rotation, translation, then the screen aspect fix (the translation comes
/// first, so in landscape the vertical centre is scaled by `W / H`).
pub(crate) fn sprite_quad(p: &QuadParams) -> [SpriteVertex; 4] {
    let (fx, fy) = (usize::from(p.flip_x), usize::from(p.flip_y));
    let mut v = [[0.0f32; 2]; 4];
    // flipping exchanges which vertex gets which corner; the UVs below do not move
    v[fx][0] = -p.sx;
    v[1 - fx][0] = p.sx;
    v[2 + fx][0] = -p.sx;
    v[3 - fx][0] = p.sx;
    v[fy * 2][1] = -p.sy;
    v[1 + fy * 2][1] = -p.sy;
    v[2 - fy * 2][1] = p.sy;
    v[3 - fy * 2][1] = p.sy;

    let (iw, ih) = (p.image.0.max(1) as f32, p.image.1.max(1) as f32);
    let ia = ih / iw;
    for q in &mut v {
        if ia < 1.0 {
            q[1] *= ia;
        } else {
            q[0] /= ia;
        }
    }
    let (sin, cos) = p.rot.sin_cos();
    for q in &mut v {
        let (x, y) = (q[0] * cos - q[1] * sin, q[0] * sin + q[1] * cos);
        *q = [x + p.x, y + p.y];
    }
    let (sw, sh) = (p.screen.0.max(1) as f32, p.screen.1.max(1) as f32);
    let sa = sw / sh;
    for q in &mut v {
        if sa > 1.0 {
            q[1] *= sa;
        } else {
            q[0] /= sa;
        }
    }
    if p.final_burn {
        let ba = sw / (sh * 4.0 / 3.0);
        for q in &mut v {
            if ba >= 1.0 {
                q[1] /= ba;
            } else {
                q[0] *= ba;
            }
        }
    }
    let (rx, ry) = (p.repeat_x, p.repeat_y);
    let uv = |k: usize| {
        let (du, dv) = (
            if k.is_multiple_of(2) { -0.5 } else { 0.5 },
            if k < 2 { -0.5 } else { 0.5 },
        );
        [du * rx + 0.5, dv * ry + 0.5]
    };
    std::array::from_fn(|k| SpriteVertex {
        pos: v[k],
        uv: uv(k),
    })
}

/// The colour key of a static image: a texel whose colour is `key` (`0xRRGGBB`) and whose alpha is 255
/// (images without alpha count as opaque) becomes transparent black, before the mip chain is built.
pub(crate) fn apply_color_key(rgba8: &mut [u8], key: u32) {
    let k = [(key >> 16) as u8, (key >> 8) as u8, key as u8, 255];
    for px in rgba8.as_chunks_mut::<4>().0 {
        if *px == k {
            px.fill(0);
        }
    }
}

/// One loaded preset sprite.
pub(crate) struct SpriteRt {
    cfg: PresetSprite,
    ctx: Context,
    vars: Vars,
    prog: Option<Program>,
    /// Pixel size of the image file (frame 0 of a GIF).
    image: (u32, u32),
    /// GIF frame delays in seconds (empty for a static image).
    delays: Vec<f64>,
    start_time: f64,
    start_frame: u32,
    /// `done` was set: the sprite is gone (its image stays registered with the renderer).
    dead: bool,
}

impl SpriteRt {
    /// Compile the sprite's code and set the variable defaults. A code part that does not compile is not run
    /// (MD3 stays silent about it); the error is returned next to the sprite for the report.
    pub(crate) fn new(
        cfg: PresetSprite,
        image: (u32, u32),
        delays: Vec<f64>,
        start: (f64, u32),
        gm: &Arc<GlobalMemory>,
        rules: &CompatRules,
    ) -> (Self, Option<String>) {
        let mut ctx = Context::new(gm.clone());
        ctx.reset_vars();
        let v = Vars::register(&mut ctx);
        for (var, value) in [
            (v.x, 0.5),
            (v.y, 0.5),
            (v.sx, 1.0),
            (v.sy, 1.0),
            (v.repeatx, 1.0),
            (v.repeaty, 1.0),
            (v.r, 1.0),
            (v.g, 1.0),
            (v.b, 1.0),
            (v.a, 1.0),
            (v.burn, 1.0),
        ] {
            ctx.set(var, value);
        }
        // MD3 runs a sprite whose code does not compile without the code, silently; the warning is ours.
        let mut dropped = Vec::new();
        let prog = compile_part(&mut ctx, &cfg.code, rules, "sprite code", &mut dropped)
            .unwrap_or_else(|e| {
                dropped.push(e.to_string());
                None
            });
        let warning = dropped.pop();
        (
            Self {
                cfg,
                ctx,
                vars: v,
                prog,
                image,
                delays,
                start_time: start.0,
                start_frame: start.1,
                dead: false,
            },
            warning,
        )
    }

    /// Evaluate the sprite for one frame; `None` when it is not drawn (killed by `done`, or a layer outside
    /// `0..=4`, which also keeps its code from running).
    pub(crate) fn frame(&mut self, inp: &SpriteInputs, screen: (u32, u32)) -> Option<SpriteDraw> {
        if self.dead || !(0..=4).contains(&self.cfg.layer) {
            return None;
        }
        let v = &self.vars;
        let ctx = &mut self.ctx;
        let since_start = inp.time - self.start_time;
        for (var, value) in [
            (v.time, since_start),
            (v.frame, f64::from(inp.frame.wrapping_sub(self.start_frame))),
            (v.fps, f64::from(inp.fps)),
            (v.progress, f64::from(inp.progress)),
            (v.bass, f64::from(inp.bass)),
            (v.mid, f64::from(inp.mid)),
            (v.treb, f64::from(inp.treb)),
            (v.bass_att, f64::from(inp.bass_att)),
            (v.mid_att, f64::from(inp.mid_att)),
            (v.treb_att, f64::from(inp.treb_att)),
        ] {
            ctx.set(var, value);
        }
        if let Some(p) = &self.prog {
            ctx.run(p);
        }
        let get = |var: Var| ctx.get(var) as f32;
        let c = &self.cfg;

        let done = ctx.get(v.done) != 0.0;
        let burn = c.burn == 1 || (c.burn != 0 && ctx.get(v.burn) != 0.0);

        let mut rot = get(v.rot);
        let spin = inp.time as f32 * c.speed;
        if c.rot_mode == 1.0 {
            rot += spin;
        } else if c.rot_mode == 2.0 {
            rot -= spin;
        }
        let params = QuadParams {
            x: clampf(get(v.x) * 2.0 - 1.0, -1000.0, 1000.0) + c.x,
            y: clampf(get(v.y) * 2.0 - 1.0, -1000.0, 1000.0) + c.y,
            sx: clampf(get(v.sx), -1000.0, 1000.0) + c.sx,
            sy: clampf(get(v.sy), -1000.0, 1000.0) + c.sy,
            rot,
            flip_x: ctx.get(v.flipx) != 0.0,
            flip_y: ctx.get(v.flipy) != 0.0,
            repeat_x: clampf(get(v.repeatx), 0.01, 100.0) * c.repeat_x,
            repeat_y: clampf(get(v.repeaty), 0.01, 100.0) * c.repeat_y,
            image: self.image,
            screen,
            final_burn: done && burn,
        };
        let rgba = [
            clampf(get(v.r), 0.0, 1.0),
            clampf(get(v.g), 0.0, 1.0),
            clampf(get(v.b), 0.0, 1.0),
            clampf(get(v.a), 0.0, 1.0) * c.alpha,
        ];
        let draw = SpriteDraw {
            layer: c.layer as u8,
            image: 0,
            frame: gif_frame_index(&self.delays, since_start),
            blend: c.blend,
            rgba,
            quad: sprite_quad(&params),
            burn,
        };
        self.dead = done;
        Some(draw)
    }

    /// Whether `done` has killed the sprite.
    #[cfg(test)]
    pub(crate) fn is_dead(&self) -> bool {
        self.dead
    }
}

/// Load a preset's sprite: ask the provider for the image, register it with `renderer` (a static image keeps
/// its colour key and gets a mip chain, a GIF is neither keyed nor mipped) and build the runtime. Every problem
/// is a warning in `report`; the preset loads without its sprite.
///
/// Clears the renderer's sprite images first, so the call also removes the previous preset's sprite.
#[allow(clippy::too_many_arguments)]
pub(crate) fn load(
    provider: Option<&mut Box<dyn TextureProvider>>,
    media_cache: &mut MediaCache,
    cfg: &PresetSprite,
    renderer: &mut Renderer,
    gm: &Arc<GlobalMemory>,
    rules: &CompatRules,
    start: (f64, u32),
    report: &mut PresetReport,
) -> Option<SpriteRt> {
    renderer.clear_sprite_images();
    let Some(provider) = provider else {
        report
            .warnings
            .push(format!("sprite {}: no texture provider", cfg.name));
        return None;
    };
    let Some(media) = media_cache.sprite(&mut **provider, &cfg.name) else {
        report
            .warnings
            .push(format!("sprite {}: image not found", cfg.name));
        return None;
    };
    let (size, delays, result) = match &*media {
        Media::Still(img) => {
            if img.width == 0
                || img.height == 0
                || img.rgba8.len() != img.width as usize * img.height as usize * 4
            {
                report
                    .warnings
                    .push(format!("sprite {}: unusable image", cfg.name));
                return None;
            }
            // the colour key is the preset's: the shared decode stays as the provider returned it
            let mut rgba8 = img.rgba8.clone();
            apply_color_key(&mut rgba8, cfg.color_key);
            let r = renderer.set_sprite_image(0, img.width, img.height, &[&rgba8], true);
            ((img.width, img.height), Vec::new(), r)
        }
        Media::Animated(a) => {
            let frames: Vec<&[u8]> = a.frames.iter().map(|f| f.rgba8.as_slice()).collect();
            let r = renderer.set_sprite_image(0, a.width, a.height, &frames, false);
            ((a.width, a.height), a.delays(), r)
        }
    };
    if let Err(e) = result {
        report.warnings.push(format!("sprite {}: {e}", cfg.name));
        return None;
    }
    let (rt, warning) = SpriteRt::new(cfg.clone(), size, delays, start, gm, rules);
    report.warnings.extend(warning);
    report.sprite = Some(cfg.name.clone());
    Some(rt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lactea_eel::Dialect;
    use lactea_render::Compat;

    /// The memory the engine hands sprite VMs under MD3: ns-eel2.
    fn md3_memory() -> Arc<GlobalMemory> {
        Arc::new(GlobalMemory::with_dialect(Dialect::NsEel2))
    }

    fn quad(p: QuadParams) -> [[f32; 2]; 4] {
        sprite_quad(&p).map(|v| v.pos)
    }

    /// Defaults of a sprite with default code and `SpriteSX = SpriteSY = -0.5`.
    fn base() -> QuadParams {
        QuadParams {
            x: 0.0,
            y: 0.0,
            sx: 0.5,
            sy: 0.5,
            rot: 0.0,
            flip_x: false,
            flip_y: false,
            repeat_x: 1.0,
            repeat_y: 1.0,
            image: (256, 256),
            screen: (1920, 1080),
            final_burn: false,
        }
    }

    fn close(a: [[f32; 2]; 4], b: [[f32; 2]; 4]) {
        for k in 0..4 {
            for c in 0..2 {
                assert!((a[k][c] - b[k][c]).abs() < 1e-3, "{a:?} vs {b:?}");
            }
        }
    }

    /// Pixel position of a clip coordinate (`y` down).
    fn pixel(p: [f32; 2], w: f32, h: f32) -> [f32; 2] {
        [(p[0] + 1.0) / 2.0 * w, (p[1] + 1.0) / 2.0 * h]
    }

    #[test]
    fn g1_square_image_fills_the_height_of_a_landscape_screen() {
        let q = quad(base());
        close(
            q,
            [
                [-0.5, -0.8889],
                [0.5, -0.8889],
                [-0.5, 0.8889],
                [0.5, 0.8889],
            ],
        );
        let (a, d) = (pixel(q[0], 1920.0, 1080.0), pixel(q[3], 1920.0, 1080.0));
        assert!(
            (a[0] - 480.0).abs() < 0.5 && (a[1] - 60.0).abs() < 0.5,
            "{a:?}"
        );
        assert!(
            (d[0] - 1440.0).abs() < 0.5 && (d[1] - 1020.0).abs() < 0.5,
            "{d:?}"
        );
    }

    #[test]
    fn g2_wide_and_g3_tall_images() {
        close(
            quad(QuadParams {
                image: (512, 256),
                ..base()
            }),
            [
                [-0.5, -0.4444],
                [0.5, -0.4444],
                [-0.5, 0.4444],
                [0.5, 0.4444],
            ],
        );
        close(
            quad(QuadParams {
                image: (256, 512),
                ..base()
            }),
            [
                [-0.25, -0.8889],
                [0.25, -0.8889],
                [-0.25, 0.8889],
                [0.25, 0.8889],
            ],
        );
    }

    #[test]
    fn g4_position_and_size_from_the_variables() {
        // x = .75, y = .25 -> (0.5, -0.5); sx = sy = .2; 1000x1000 target
        let q = quad(QuadParams {
            x: 0.5,
            y: -0.5,
            sx: 0.2,
            sy: 0.2,
            screen: (1000, 1000),
            ..base()
        });
        close(q, [[0.3, -0.7], [0.7, -0.7], [0.3, -0.3], [0.7, -0.3]]);
        let p = pixel(q[0], 1000.0, 1000.0);
        assert!((p[0] - 650.0).abs() < 0.5 && (p[1] - 150.0).abs() < 0.5);
    }

    #[test]
    fn g5_quarter_turn_g6_flip() {
        close(
            quad(QuadParams {
                rot: std::f32::consts::FRAC_PI_2,
                screen: (1000, 1000),
                ..base()
            }),
            [[0.5, -0.5], [0.5, 0.5], [-0.5, -0.5], [-0.5, 0.5]],
        );
        close(
            quad(QuadParams {
                flip_x: true,
                sy: 0.25,
                screen: (1000, 1000),
                ..base()
            }),
            [[0.5, -0.25], [-0.5, -0.25], [0.5, 0.25], [-0.5, 0.25]],
        );
        // flipping y exchanges the rows, the UVs stay
        let flipped = sprite_quad(&QuadParams {
            flip_y: true,
            screen: (1000, 1000),
            ..base()
        });
        assert_eq!(flipped[0].pos, [-0.5, 0.5]);
        assert_eq!(flipped[2].pos, [-0.5, -0.5]);
        assert_eq!(flipped[0].uv, [0.0, 0.0]);
    }

    #[test]
    fn g7_landscape_scales_the_vertical_centre() {
        let q = quad(QuadParams {
            y: -0.5,
            sx: 0.2,
            sy: 0.2,
            screen: (1600, 900),
            ..base()
        });
        assert!((q[0][1] + 1.2444).abs() < 1e-3, "{q:?}");
        assert!((q[2][1] + 0.5333).abs() < 1e-3, "{q:?}");
        assert!(((q[0][1] + q[2][1]) / 2.0 + 0.8889).abs() < 1e-3);
    }

    #[test]
    fn g8_rotation_mode_adds_time_times_speed() {
        // var rot 0.1 + 2.0 * 1.5 = 3.1
        close(
            quad(QuadParams {
                rot: 0.1 + 3.0,
                screen: (1000, 1000),
                ..base()
            }),
            [
                [0.5204, 0.4788],
                [-0.4788, 0.5204],
                [0.4788, -0.5204],
                [-0.5204, -0.4788],
            ],
        );
    }

    #[test]
    fn g9_gif_with_a_positive_size_offset_overscans() {
        let q = quad(QuadParams {
            sx: 1.16,
            sy: 1.16,
            image: (450, 500),
            ..base()
        });
        assert!((q[1][0] - 1.044).abs() < 1e-3, "{q:?}");
        assert!((q[3][1] - 2.0622).abs() < 1e-3, "{q:?}");
    }

    #[test]
    fn repeat_scales_the_uvs_around_the_centre_and_final_burn_corrects_for_4_3() {
        let v = sprite_quad(&QuadParams {
            repeat_x: 2.0,
            repeat_y: 0.5,
            ..base()
        });
        assert_eq!(v[0].uv, [-0.5, 0.25]);
        assert_eq!(v[3].uv, [1.5, 0.75]);
        // 1920x1080 is wider than 4:3: the last burning frame shrinks y by W / (H * 4/3)
        let plain = quad(base());
        let burn = quad(QuadParams {
            final_burn: true,
            ..base()
        });
        assert!((burn[0][1] - plain[0][1] / (1920.0 / (1080.0 * 4.0 / 3.0))).abs() < 1e-4);
        assert_eq!(burn[0][0], plain[0][0]);
    }

    #[test]
    fn color_key_clears_only_opaque_texels_of_that_colour() {
        let mut px = vec![
            0, 0, 0, 255, // keyed
            0, 0, 0, 128, // same colour, translucent: kept
            1, 0, 0, 255, // other colour: kept
            0, 0, 0, 0, // already transparent: kept as is
        ];
        apply_color_key(&mut px, 0);
        assert_eq!(&px[..4], &[0, 0, 0, 0]);
        assert_eq!(&px[4..8], &[0, 0, 0, 128]);
        assert_eq!(&px[8..12], &[1, 0, 0, 255]);
        let mut px = vec![0x12, 0x34, 0x56, 255, 0x56, 0x34, 0x12, 255];
        apply_color_key(&mut px, 0x123456);
        assert_eq!(px, [0, 0, 0, 0, 0x56, 0x34, 0x12, 255]);
    }

    fn sprite(code: &str, f: impl FnOnce(&mut PresetSprite)) -> SpriteRt {
        let mut cfg = PresetSprite {
            name: "sprites\\t.png".into(),
            ..PresetSprite::default()
        };
        cfg.code = code
            .split('\n')
            .map(|l| format!("{l}\u{1}"))
            .collect::<String>();
        f(&mut cfg);
        let gm = md3_memory();
        let (rt, warning) = SpriteRt::new(
            cfg,
            (256, 256),
            Vec::new(),
            (10.0, 100),
            &gm,
            &Compat::MilkDrop3.rules(),
        );
        assert!(warning.is_none(), "{warning:?}");
        rt
    }

    fn inputs(time: f64) -> SpriteInputs {
        SpriteInputs {
            time,
            frame: 160,
            fps: 60.0,
            ..SpriteInputs::default()
        }
    }

    #[test]
    fn default_sprite_is_centred_and_half_size_with_the_preset_offsets() {
        let mut s = sprite("", |_| {});
        let d = s.frame(&inputs(11.0), (1000, 1000)).unwrap();
        // x = y = .5, sx = sy = 1, SpriteSX/SY = -0.5
        close(
            d.quad.map(|v| v.pos),
            [[-0.5, -0.5], [0.5, -0.5], [-0.5, 0.5], [0.5, 0.5]],
        );
        assert_eq!(d.rgba, [1.0; 4]);
        assert_eq!((d.layer, d.blend, d.burn, d.frame), (0, 4, true, 0));
    }

    #[test]
    fn code_sees_time_and_frame_since_the_start_and_drives_the_draw() {
        let mut s = sprite(
            "x = time;\ny = frame / 100;\na = 0.5;\nr = 2;\nblendmode = 9;",
            |c| {
                c.alpha = 0.5;
                c.x = 0.25;
                c.sx = -0.8;
                c.sy = -0.8;
            },
        );
        let d = s.frame(&inputs(10.5), (1000, 1000)).unwrap();
        // x = 0.5 -> 0; y = 60 / 100 = 0.6 -> 0.2; plus SpriteX; the size offsets only change the extent
        let cx = (d.quad[0].pos[0] + d.quad[1].pos[0]) / 2.0;
        let cy = (d.quad[0].pos[1] + d.quad[2].pos[1]) / 2.0;
        assert!((cx - 0.25).abs() < 1e-5, "{cx}");
        assert!((cy - 0.2).abs() < 1e-5, "{cy}");
        // r is clamped to 1, a = 0.5 * SpriteAlpha (not clamped afterwards); SpriteBlend wins over `blendmode`
        assert_eq!(d.rgba, [1.0, 1.0, 1.0, 0.25]);
        assert_eq!(d.blend, 4);
    }

    #[test]
    fn rotation_mode_one_and_two_spin_with_the_absolute_time() {
        let angle_at = |mode: f32, time: f64| {
            let mut s = sprite("rot = 0.1;", |c| {
                c.rot_mode = mode;
                c.speed = 1.5;
            });
            let d = s.frame(&inputs(time), (1000, 1000)).unwrap();
            // the first vertex of a 0.5 half-size square, rotated: recover the angle from v1 - v0
            let (a, b) = (d.quad[0].pos, d.quad[1].pos);
            (b[1] - a[1]).atan2(b[0] - a[0])
        };
        let want = |rot: f32| {
            let r = rot.rem_euclid(std::f32::consts::TAU);
            if r > std::f32::consts::PI {
                r - std::f32::consts::TAU
            } else {
                r
            }
        };
        assert!(
            (angle_at(0.0, 2.0) - 0.1).abs() < 1e-4,
            "no mode: the variable only"
        );
        assert!((angle_at(1.0, 2.0) - want(0.1 + 3.0)).abs() < 1e-4);
        assert!((angle_at(2.0, 2.0) - want(0.1 - 3.0)).abs() < 1e-4);
        // an exact comparison: 1.0000001 is not mode 1
        assert!((angle_at(1.000_000_1, 2.0) - 0.1).abs() < 1e-4);
    }

    #[test]
    fn burn_follows_spriteburn_and_the_variable() {
        let burns = |mode: i32, var: f64| {
            let mut s = sprite(&format!("burn = {var};"), |c| c.burn = mode);
            s.frame(&inputs(11.0), (1000, 1000)).unwrap().burn
        };
        assert!(burns(1, 0.0), "SpriteBurn=1 always burns");
        assert!(!burns(0, 1.0), "SpriteBurn=0 never burns");
        assert!(
            burns(2, 1.0) && !burns(2, 0.0),
            "any other value follows the variable"
        );
    }

    #[test]
    fn done_kills_the_sprite_after_its_last_draw() {
        let mut s = sprite("done = time > 1;", |_| {});
        assert!(s.frame(&inputs(10.5), (640, 360)).is_some());
        assert!(
            s.frame(&inputs(11.5), (640, 360)).is_some(),
            "the frame that sets done is drawn"
        );
        assert!(s.is_dead());
        assert!(s.frame(&inputs(12.0), (640, 360)).is_none());
    }

    #[test]
    fn a_layer_outside_0_to_4_is_never_drawn_and_its_code_never_runs() {
        let mut s = sprite("done = 1;", |c| c.layer = 5);
        assert!(s.frame(&inputs(11.0), (640, 360)).is_none());
        assert!(
            !s.is_dead(),
            "the code did not run, so done never killed it"
        );
    }

    #[test]
    fn gif_frames_follow_the_time_since_the_sprite_started() {
        let gm = md3_memory();
        let cfg = PresetSprite {
            name: "g.gif".into(),
            ..PresetSprite::default()
        };
        let (mut s, _) = SpriteRt::new(
            cfg,
            (4, 4),
            vec![0.1, 0.2, 0.05],
            (10.0, 0),
            &gm,
            &Compat::MilkDrop3.rules(),
        );
        let frame_at = |s: &mut SpriteRt, t: f64| s.frame(&inputs(t), (640, 360)).unwrap().frame;
        assert_eq!(frame_at(&mut s, 10.0), 0);
        assert_eq!(frame_at(&mut s, 10.12), 1);
        assert_eq!(frame_at(&mut s, 10.31), 2);
        assert_eq!(
            frame_at(&mut s, 10.36),
            0,
            "loops from the sprite start, not from time 0"
        );
    }

    #[test]
    fn broken_code_is_reported_and_not_run() {
        let gm = md3_memory();
        let cfg = PresetSprite {
            name: "a.png".into(),
            code: "x = (;\u{1}".into(),
            ..PresetSprite::default()
        };
        let (mut s, warning) = SpriteRt::new(
            cfg,
            (4, 4),
            Vec::new(),
            (0.0, 0),
            &gm,
            &Compat::MilkDrop3.rules(),
        );
        assert!(warning.is_some());
        assert!(s.frame(&inputs(1.0), (640, 360)).is_some());
    }
}
