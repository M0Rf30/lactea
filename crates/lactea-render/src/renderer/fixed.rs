// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-FileCopyrightText: 2005-2013 Nullsoft, Inc.
// SPDX-License-Identifier: (MIT OR Apache-2.0) AND BSD-3-Clause

//! Fixed-function geometry: overlay, waves and the no-shader composite.

use super::*;

impl Renderer {
    pub(super) fn build_overlay(
        &self,
        b: &mut Batch,
        f: &FrameInput,
        aspect: (f32, f32),
        pm_wave: Option<wavegen::PmWaveGeom>,
        scr: &mut FrameScratch,
    ) {
        let rules = &self.rules;
        let (w, h) = (self.cfg.width as f32, self.cfg.height as f32);
        let width = self.cfg.width;
        let (asp_x, asp_y) = aspect;
        let inv_aspect = (1.0 / asp_x, 1.0 / asp_y);
        let mode = self.stroke_mode();

        // ---- custom shapes ----
        // scratch shared by every instance: a preset can have thousands
        let mut g = geom::ShapeGeom::default();
        let mut tri: Vec<MeshVertex> = Vec::new();
        let mut edge: Vec<[f32; 2]> = Vec::new();
        let mut segs: Vec<WfVertex> = Vec::new();
        for (si, s) in f.shapes.iter().enumerate() {
            // a textured shape with an `image=` samples that user texture; its texture aspect is 1
            let image = s.image.as_ref().filter(|_| s.textured);
            geom::shape_geometry_into(
                &mut g,
                s,
                asp_y,
                if image.is_some() { 1.0 } else { asp_y },
                rules.max_shape_sides,
                s.textured,
            );
            let am = f
                .md2_blend
                .as_ref()
                .and_then(|m| m.shape_alpha_mult.get(si).copied())
                .unwrap_or(1.0);
            let c1 = geom::conv_color_alpha_mult(rules, s.rgba, am);
            let c2 = geom::conv_color_alpha_mult(rules, s.rgba2, am);
            let blend = if s.additive {
                Blend::SrcAlphaAdd
            } else {
                Blend::SrcAlpha
            };
            if s.textured {
                // MD2 textures the fill with the previous frame; uv per vertex
                tri.clear();
                tri.reserve(3 * g.pts.len().saturating_sub(2));
                for i in 1..g.pts.len() - 1 {
                    for k in [0, i, i + 1] {
                        tri.push(MeshVertex {
                            pos: g.pts[k],
                            diffuse: if k == 0 { c1 } else { c2 },
                            uv: [g.uv[k][0], g.uv[k][1], 0.0, 0.0],
                            rad_ang: [0.0; 2],
                        });
                    }
                }
                // no `gl_fill_ties` here: the nearest-sampled UV of a moved vertex flips texel choices at pixel centres
                // (`Zylot - Star Ornament` main 79.0 -> 72.2 dB)
                let tex_blend = if s.additive {
                    Blend::SrcAlphaAdd
                } else {
                    Blend::SrcAlpha
                };
                if let Some(im) = image {
                    // projectM `CustomShape::Draw`: the `image=` texture with its own sampler instead of the main image
                    b.tex_image(tex_blend, &tri, im);
                } else if si == 0 && rules.shape_main_nearest && self.last_blur_levels > 0 {
                    // projectM: `BlurTexture::Update` (only runs when a shader samples blur) leaves its (linear,
                    // clamp-to-edge) sampler bound on unit 0; the first shape draw consumes it and
                    // `Sampler::Unbind(0)` follows, so every later textured draw falls back to the texture's own
                    // NEAREST/REPEAT parameters. `last_blur_levels` is the *native* level: a diagnostic blur chain
                    // must not trigger it (projectM restores the sampler after its own forced blur).
                    b.tex_leak(tex_blend, &tri);
                } else {
                    b.tex(tex_blend, &tri);
                }
            } else {
                if rules.gl_fill_ties {
                    let colors: Vec<[f32; 4]> = (0..g.pts.len())
                        .map(|k| if k == 0 { c1 } else { c2 })
                        .collect();
                    let mut tris = geom::fan_to_list(&g.pts, &colors);
                    geom::gl_fill_ties(&mut tris, w, h);
                    b.flat(blend, &tris);
                } else {
                    b.flat_extend(Prim::Tri, blend, |out| {
                        geom::push_fan(out, &g.pts, c1, c2);
                    });
                }
            }
            // projectM `CustomShape::Draw`: `border_a > 0.0001f`; MD2: `var_pf_border_a > 0`
            let border_gate = if rules.pm_waveforms { 0.0001 } else { 0.0 };
            if s.border_rgba[3] > border_gate {
                let bc = geom::conv_color_raw_alpha_mult(rules, s.border_rgba, am);
                let its = if s.thick { 4 } else { 1 };
                let (inc_x, inc_y) = if rules.shape_thick_full_pixel {
                    (2.0 / w, 2.0 / h)
                } else {
                    (1.0 / w, 1.0 / h)
                };
                edge.clear();
                edge.extend_from_slice(&g.pts[1..]); // sides + 1 points (closed)
                for it in 0..its {
                    match it {
                        1 => edge.iter_mut().for_each(|p| p[0] += inc_x),
                        2 => edge.iter_mut().for_each(|p| p[1] -= inc_y),
                        3 => edge.iter_mut().for_each(|p| p[0] -= inc_x),
                        _ => {}
                    }
                    geom::strip_segments_into(&mut segs, &edge, |_| bc);
                    b.segments(mode, LineRule::Diamond, blend, w, h, &segs);
                }
            }
        }
        b.mark(0);

        // ---- custom waves ----
        for (wave_i, cw) in f.waves.iter().enumerate() {
            let wam = f
                .md2_blend
                .as_ref()
                .and_then(|m| m.wave_alpha_mult.get(wave_i).copied())
                .unwrap_or(1.0);
            let n = cw.points.len();
            if n < 2 && !(cw.use_dots && n >= 1) {
                continue;
            }
            scr.pts.clear();
            scr.cols.clear();
            for p in &cw.points {
                scr.pts.push(geom::wave_point_clip(rules, p, inv_aspect));
                scr.cols
                    .push(geom::conv_color_alpha_mult(rules, p.rgba, wam));
            }
            let blend = if cw.additive {
                Blend::SrcAlphaAdd
            } else {
                Blend::SrcAlpha
            };
            let (x_inc, y_inc) = if rules.wave_thick_ndc_inc {
                (1.0 / w, 1.0 / w)
            } else {
                (2.0 / w, 2.0 / h)
            };
            if cw.use_dots {
                // projectM draws every vertex of SmoothWave (the n source points *and* the n-1 interpolated ones)
                // as GL_POINTS and ignores gl_PointSize (always 1 px)
                let (pp, pc): (&[[f32; 2]], &[[f32; 4]]) = if rules.wave_dots_smoothed {
                    smooth_with_colors_into(&mut scr.sp, &mut scr.sc, &scr.pts, &scr.cols);
                    (&scr.sp, &scr.sc)
                } else {
                    (&scr.pts, &scr.cols)
                };
                let size = if rules.point_size_honored {
                    (if width >= 1024 { 2.0 } else { 1.0 }) + if cw.thick { 1.0 } else { 0.0 }
                } else {
                    1.0
                };
                scr.verts.clear();
                scr.verts.extend(
                    pp.iter()
                        .zip(pc)
                        .map(|(p, c)| WfVertex { pos: *p, color: *c }),
                );
                b.points(mode, blend, w, h, &scr.verts, size);
            } else {
                // SmoothWave interpolates positions; colours follow the source point (vo[j+1].Diffuse = vi[i].Diffuse)
                smooth_with_colors_into(&mut scr.sp, &mut scr.sc, &scr.pts, &scr.cols);
                let its = if cw.thick { 4 } else { 1 };
                for it in 0..its {
                    match it {
                        1 => scr.sp.iter_mut().for_each(|p| p[0] += x_inc),
                        // the custom waves step y by `-y_inc` (projectM's main waveform: `+y_inc`)
                        2 => scr.sp.iter_mut().for_each(|p| p[1] -= y_inc),
                        3 => scr.sp.iter_mut().for_each(|p| p[0] -= x_inc),
                        _ => {}
                    }
                    let sc = &scr.sc;
                    geom::strip_segments_into(&mut scr.verts, &scr.sp, |i| sc[i]);
                    b.segments(mode, LineRule::Strip, blend, w, h, &scr.verts);
                }
            }
        }
        b.mark(1);

        // ---- main wave ----
        self.build_main_wave(b, f, aspect, pm_wave, scr);

        // ---- darken center ----
        if f.darken_center {
            geom::darken_center_triangles(&mut scr.verts, asp_y, rules);
            b.flat(Blend::SrcAlpha, &scr.verts);
        }

        // ---- borders ----
        for it in 0..2 {
            let (size_rgba, label_a) = if it == 0 {
                (f.borders.ob_rgba, f.borders.ob_rgba[3])
            } else {
                (f.borders.ib_rgba, f.borders.ib_rgba[3])
            };
            if label_a > 0.001 && !f.borders.hidden {
                let c = geom::conv_color_raw(rules, size_rgba);
                let (ob, ib) = (f.borders.ob_size, f.borders.ib_size);
                let tris = &mut scr.verts;
                if rules.pm_border_mesh {
                    geom::border_triangles(tris, it, ob, ib, c);
                } else {
                    geom::border_triangles_md2(tris, it, ob, ib, c);
                }
                if rules.gl_fill_ties {
                    geom::gl_fill_ties(tris, w, h);
                }
                b.flat(Blend::SrcAlpha, tris);
            }
        }
    }

    /// One `DrawWave` pass of the MD2 family: the MD2 port, or MilkDrop 3's 17 modes (`md3_wave_modes`).
    pub(super) fn main_wave_raw(
        &self,
        m: &crate::frame::MainWave,
        wl: &[f32],
        wr: &[f32],
        spec_l: &[f32],
        spec_r: &[f32],
        f: &FrameInput,
        aspect: (f32, f32),
        blending: bool,
    ) -> Option<wavegen::WaveGeom> {
        if self.rules.md3_wave_modes {
            return wavegen_md3::main_wave_geometry_md3_raw(
                m,
                wl,
                wr,
                spec_l,
                f.time as f32,
                [f.bass, f.mid, f.treb],
                f.imm_abs,
                self.cfg.width,
                aspect,
                blending,
            );
        }
        wavegen::main_wave_geometry_raw(
            m,
            wl,
            wr,
            spec_l,
            spec_r,
            f.time as f32,
            [f.bass, f.mid, f.treb],
            f.imm_abs,
            self.cfg.width,
            aspect,
            blending,
        )
    }

    /// `DrawWave` while MD2 blends two presets: the new wave (`it == 0`) and, if the wave mode changed, the old
    /// preset's mode drawn from the same parameters (`it == 1`), mixed by `CosineInterp(progress)`; alpha is blended
    /// the same way and the "don't draw if alpha < 0.004" test applies to the result. A mode this port does not
    /// draw (BeatDrop's modes beyond 8) cannot be blended: the new wave is then drawn alone, an unsupported new mode
    /// draws nothing.
    pub(super) fn main_wave_geometry_blend(
        &self,
        f: &FrameInput,
        bl: &blend::Md2Blend,
        wl: &[f32],
        wr: &[f32],
        aspect: (f32, f32),
    ) -> Option<wavegen::WaveGeom> {
        let mw = &f.main_wave;
        let call = |m: &crate::frame::MainWave| {
            self.main_wave_raw(m, wl, wr, &mw.spectrum[0], &mw.spectrum[1], f, aspect, true)
        };
        let new = call(mw)?;
        let mut out = new.clone();
        if let Some(old_mode) = bl.old_wave_mode {
            let mut old_mw = mw.clone();
            old_mw.mode = old_mode;
            if let Some(old) = call(&old_mw) {
                let mix = blend::cosine_interp(bl.progress);
                let line = blend::blend_waves(
                    &blend::WaveLine {
                        verts: new.verts.clone(),
                        brk: new.brk,
                    },
                    &blend::WaveLine {
                        verts: old.verts,
                        brk: old.brk,
                    },
                    mix,
                );
                out.verts = line.verts;
                out.brk = line.brk;
                out.alpha = blend::blend_wave_alpha(new.alpha, old.alpha, mix);
            }
        }
        (out.alpha >= 0.004).then_some(out)
    }

    pub(super) fn build_main_wave(
        &self,
        b: &mut Batch,
        f: &FrameInput,
        aspect: (f32, f32),
        pm_wave: Option<wavegen::PmWaveGeom>,
        scr: &mut FrameScratch,
    ) {
        let rules = &self.rules;
        let mode = self.stroke_mode();
        let mw = &f.main_wave;
        let (w, h) = (self.cfg.width as f32, self.cfg.height as f32);
        if rules.pm_waveforms {
            if let Some(g) = pm_wave {
                self.build_main_wave_pm(b, f, g, scr);
            }
            return;
        }
        let (wl, wr) = wavegen::prepare_main_waveform(mw);
        let geometry = match f.md2_blend.as_ref() {
            None => self
                .main_wave_raw(
                    mw,
                    &wl,
                    &wr,
                    &mw.spectrum[0],
                    &mw.spectrum[1],
                    f,
                    aspect,
                    false,
                )
                .filter(|g| g.alpha >= 0.004),
            Some(bl) => self.main_wave_geometry_blend(f, bl, &wl, &wr, aspect),
        };
        let Some(g) = geometry else {
            return;
        };
        let color = geom::conv_color_raw(rules, [g.rgb[0], g.rgb[1], g.rgb[2], g.alpha]);
        let blend = if mw.additive {
            Blend::SrcAlphaAdd
        } else {
            Blend::SrcAlpha
        };

        // TESSELLATE (SmoothWave, per strip): up to two strips, the second empty without a break
        let (first, second) = match g.brk {
            None => (&g.verts[..], &g.verts[..0]),
            Some(k) => (&g.verts[..k], &g.verts[k..]),
        };
        wavegen::smooth_wave_into(&mut scr.pts, first);
        wavegen::smooth_wave_into(&mut scr.sp, second);
        // MD2 only draws the thick / dots passes of the main wave from a texture size of 512 (`thick_needs_512`);
        // projectM (`build_main_wave_pm`) always draws them
        let its = if (mw.thick || mw.usedots) && (!rules.thick_needs_512 || self.cfg.width >= 512) {
            4
        } else {
            1
        };
        let (x_inc, y_inc) = (2.0 / w, 2.0 / h);
        for it in 0..its {
            for s in [&mut scr.pts, &mut scr.sp] {
                match it {
                    1 => s.iter_mut().for_each(|p| p[0] += x_inc),
                    2 => s.iter_mut().for_each(|p| p[1] -= y_inc),
                    3 => s.iter_mut().for_each(|p| p[0] -= x_inc),
                    _ => {}
                }
            }
            scr.verts.clear();
            for s in [&scr.pts, &scr.sp] {
                if mw.usedots {
                    scr.verts
                        .extend(s.iter().map(|&pos| WfVertex { pos, color }));
                } else {
                    geom::strip_segments_into(&mut scr.seg, s, |_| color);
                    scr.verts.extend_from_slice(&scr.seg);
                }
            }
            if mw.usedots {
                b.points(mode, blend, w, h, &scr.verts, 1.0);
            } else {
                b.segments(mode, LineRule::Strip, blend, w, h, &scr.verts);
            }
        }
    }

    /// projectM main waveform (`Waveform::Draw`): vertices from [`wavegen::main_wave_geometry_projectm`] (computed by
    /// [`Renderer::advance_main_wave_pm`] every frame), line strip / loop, thick = 4 passes at (0,0) (+x) (+x,+y) (+y)
    /// pixel offsets.
    pub(super) fn build_main_wave_pm(
        &self,
        b: &mut Batch,
        f: &FrameInput,
        g: wavegen::PmWaveGeom,
        scr: &mut FrameScratch,
    ) {
        let rules = &self.rules;
        let mode = self.stroke_mode();
        let mw = &f.main_wave;
        let (w, h) = (self.cfg.width as f32, self.cfg.height as f32);
        if g.alpha <= 0.0 {
            return;
        }

        let color = geom::conv_color_raw(rules, [g.rgb[0], g.rgb[1], g.rgb[2], g.alpha]);
        let blend = if mw.additive {
            Blend::SrcAlphaAdd
        } else {
            Blend::SrcAlpha
        };
        let its = if mw.thick || mw.usedots { 4 } else { 1 };
        let (x_inc, y_inc) = (2.0 / w, 2.0 / h);
        for mut strip in g.strips {
            for it in 0..its {
                match it {
                    1 => strip.iter_mut().for_each(|p| p[0] += x_inc),
                    // projectM's main waveform steps y by `+y_inc` (the custom waves and MD2's main wave use `-y_inc`)
                    2 => strip.iter_mut().for_each(|p| p[1] += y_inc),
                    3 => strip.iter_mut().for_each(|p| p[0] -= x_inc),
                    _ => {}
                }
                if mw.usedots {
                    scr.verts.clear();
                    scr.verts
                        .extend(strip.iter().map(|&pos| WfVertex { pos, color }));
                    b.points(mode, blend, w, h, &scr.verts, 1.0);
                } else if strip.iter().any(|p| *p != strip[0]) {
                    // A strip whose points are all equal has zero-length segments, which `LineRule::Strip` lights no
                    // pixel for (`stroke::strip_segment` returns on `dx == dy == 0` after the same viewport snap, and
                    // equal floats snap equally), so skipping it only saves the work: pixel-identical to projectM's
                    // `Waveform.cpp:100`, which draws every strip.
                    // Waveform::Draw disables GL_LINE_SMOOTH: half-open strip rule, not diamond exit.
                    scr.pts.clear();
                    scr.pts.extend_from_slice(&strip);
                    if g.is_loop {
                        scr.pts.push(strip[0]);
                    }
                    geom::strip_segments_into(&mut scr.verts, &scr.pts, |_| color);
                    b.segments(mode, LineRule::Strip, blend, w, h, &scr.verts);
                }
            }
        }
    }

    /// `GetVertices` of projectM's main waveform. Runs every frame (also when the overlays of the frame are lost or the
    /// alpha is 0) because it advances the persistent wave-buffer tail ([`wavegen::PmWaveTail`]).
    pub(super) fn advance_main_wave_pm(&mut self, f: &FrameInput) -> Option<wavegen::PmWaveGeom> {
        wavegen::main_wave_geometry_projectm(
            &f.main_wave,
            f.time as f32,
            [f.bass, f.mid, f.treb],
            (self.cfg.width, self.cfg.height),
            &mut self.main_wave_tail,
            &mut self.scratch.pm_wave,
        )
    }

    /// `ShowToUser_NoShaders` (== projectM `VideoEcho` + `Filters`).
    pub(super) fn build_noshader_comp(&self, f: &FrameInput) -> Batch {
        let rules = &self.rules;
        let (w, h) = (self.cfg.width, self.cfg.height);
        let mut b = Batch::default();
        let p = &f.post;

        // hue-shader corner tint
        let mut shade = [[1.0f32; 3]; 4];
        let amount = p.fshader;
        if rules.hue_tint_always {
            shade = geom::hue_shade(f.time as f32, f.hue_offsets.unwrap_or(self.rot.hue_offsets));
        } else if amount > 0.001 {
            let s = geom::hue_shade(f.time as f32, f.hue_offsets.unwrap_or(self.rot.hue_offsets));
            for i in 0..4 {
                for k in 0..3 {
                    shade[i][k] = s[i][k] * amount + 1.0 * (1.0 - amount);
                }
            }
        }
        let col = |mult: f32| -> [[f32; 4]; 4] {
            let mut c = [[0.0; 4]; 4];
            for (k, out) in c.iter_mut().enumerate() {
                // corner k of `echo_quad` is bottom-left, bottom-right, top-left, top-right
                let s = if rules.shade_top_first { k ^ 2 } else { k };
                *out = geom::conv_color_raw(
                    rules,
                    [
                        mult * shade[s][0],
                        mult * shade[s][1],
                        mult * shade[s][2],
                        1.0,
                    ],
                );
            }
            c
        };
        let gamma = p.gamma;
        let orient = p.echo_orient % 4;

        let mult = if rules.echo_quad_aspect {
            geom::echo_aspect_mult(w, h)
        } else {
            [1.0, 1.0]
        };
        if p.echo_alpha > 0.001 {
            for i in 0..2 {
                let zoom = if i == 0 { 1.0 } else { p.echo_zoom };
                let lo = 0.5 - 0.5 / zoom;
                let hi = 0.5 + 0.5 / zoom;
                let mut uv = [[lo, hi], [hi, hi], [lo, lo], [hi, lo]];
                if i == 1 {
                    for t in uv.iter_mut() {
                        if orient % 2 != 0 {
                            t[0] = 1.0 - t[0];
                        }
                        if orient >= 2 {
                            t[1] = 1.0 - t[1];
                        }
                    }
                }
                let mix = if i == 1 {
                    p.echo_alpha
                } else {
                    1.0 - p.echo_alpha
                };
                b.tex(
                    if i == 0 { Blend::Replace } else { Blend::Add },
                    &geom::echo_quad(w, h, uv, col(mix), rules.echo_diag_flipped, mult),
                );
                if gamma > 0.001 {
                    let n_redraws = (gamma - 0.0001) as i32;
                    for r in 0..n_redraws {
                        let g = if r == n_redraws - 1 {
                            gamma - (gamma - 0.0001) as i32 as f32
                        } else {
                            1.0
                        };
                        b.tex(
                            Blend::Add,
                            &geom::echo_quad(w, h, uv, col(g * mix), rules.echo_diag_flipped, mult),
                        );
                    }
                }
            }
        } else {
            let uv = [[0.0, 1.0], [1.0, 1.0], [0.0, 0.0], [1.0, 0.0]];
            let n_passes = (gamma - rules.gamma_pass_eps) as i32 + 1;
            for pass in 0..n_passes {
                let g = if pass == n_passes - 1 {
                    gamma - pass as f32
                } else {
                    1.0
                };
                b.tex(
                    if pass == 0 {
                        Blend::Replace
                    } else {
                        Blend::Add
                    },
                    &geom::echo_quad(w, h, uv, col(g), rules.echo_diag_flipped, mult),
                );
            }
        }

        // brighten / darken / solarize / invert: white quads with destination-colour blends
        let white = [1.0f32; 4];
        // the split follows the echo quad's (projectM `Filters` draws the same TL,TR,BL,BR strip); flat colour, so
        // it is not observable
        let flat_quad: Vec<WfVertex> = geom::echo_quad(
            w,
            h,
            [[0.0; 2]; 4],
            [white; 4],
            rules.echo_diag_flipped,
            [1.0, 1.0],
        )
        .iter()
        .map(|m| WfVertex {
            pos: m.pos,
            color: white,
        })
        .collect();
        let quad = |blend: Blend, b: &mut Batch| b.flat(blend, &flat_quad);
        if p.brighten {
            quad(Blend::InvDstZero, &mut b);
            quad(Blend::ZeroDst, &mut b);
            quad(Blend::InvDstZero, &mut b);
        }
        if p.darken {
            quad(Blend::ZeroDst, &mut b);
        }
        if p.solarize {
            quad(Blend::ZeroInvDst, &mut b);
            quad(Blend::DstOne, &mut b);
        }
        if p.invert {
            quad(Blend::InvDstZero, &mut b);
        }
        b
    }
}
