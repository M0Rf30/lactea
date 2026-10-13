// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-FileCopyrightText: 2005-2013 Nullsoft, Inc.
// SPDX-License-Identifier: (MIT OR Apache-2.0) AND BSD-3-Clause

//! Frame orchestration: the pass sequence of one rendered frame.

use super::*;

impl Renderer {
    pub(super) fn render_frame_inner(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        input: &FrameInput,
        target: Option<&wgpu::TextureView>,
    ) -> Result<(), RenderError> {
        self.check_input(input)?;
        // before any pass is recorded: every shader of this frame (and of the old state of a blend) reads the new FFT
        if let Some(fft) = &input.ext.fft
            && !self.rules.md2_shader_inputs
        {
            self.record_fft_upload(encoder, fft);
        }
        if let Some(bd) = &input.ext.beatdrop
            && self.rules.md2_shader_inputs
        {
            self.record_beatdrop_upload(encoder, bd);
        }
        self.anim_time = input.time;
        let rules = self.rules;
        let (w, h) = (self.cfg.width, self.cfg.height);
        let prev_i = self.main_idx;
        let next_i = 1 - prev_i;
        let aspect = (self.geom.aspect_x, self.geom.aspect_y);

        // projectM: the GPU evaluates the warp UV formula (see `shaders::warp_vs`)
        let gpu_warp = if rules.warp_uv_on_gpu {
            input.warp_gpu.as_ref()
        } else {
            None
        };
        // With a GPU warp stage the CPU grid is only the motion vector lookup's fallback, which `cpu_warp_uv` derives
        // from `warp_gpu` when it is needed: a frame that supplies none costs nothing.
        let uv_unneeded =
            input.warp_uv.is_none() && gpu_warp.is_some() && input.md2_blend.is_none();
        let warp_uv: std::borrow::Cow<'_, [[f32; 2]]> = match &input.warp_uv {
            Some(u) => std::borrow::Cow::Borrowed(u),
            None if uv_unneeded => std::borrow::Cow::Borrowed(&[]),
            None => std::borrow::Cow::Owned(self.default_warp_uv(input)),
        };

        // ---- 1. motion vectors onto the previous image ---------------------------------
        let mv_visible = input.mv.rgba[3]
            >= if rules.mv_skip_first_frame {
                0.0001
            } else {
                0.001
            };
        if mv_visible && !(rules.mv_skip_first_frame && self.frames_since_resize == 0) {
            let gpu_uv = if rules.warp_uv_on_gpu && self.prev_warp_gpu.is_some() {
                self.gpu_warp_vertex_uv().ok()
            } else {
                None
            };
            let cpu_uv = if gpu_uv.is_none() && self.prev_warp_uv.is_none() {
                self.prev_warp_gpu.as_ref().map(|wg| self.cpu_warp_uv(wg))
            } else {
                None
            };
            // No previous warp data (first frame after a load / reset with no readback to fall back on): there is
            // no mesh to look vectors up in, so nothing is drawn.
            if let Some(prev_uv) = gpu_uv
                .as_ref()
                .or(self.prev_warp_uv.as_ref())
                .or(cpu_uv.as_ref())
            {
                let color = geom::conv_color_raw(&rules, input.mv.rgba);
                let mut verts = std::mem::take(&mut self.scratch.verts);
                if rules.mv_mirrored_lookup {
                    // MilkDrop 2: one D3D line list, mirrored-row lookup into the previous mesh
                    geom::motion_vector_lines(
                        &mut verts, &input.mv, &self.geom, prev_uv, w, h, &rules, color,
                    );
                } else {
                    // projectM: GL_LINES over its (never cleared) vertex buffer, see `motion.rs`
                    self.mv_state.segments(
                        &mut verts,
                        &input.mv,
                        &self.geom,
                        prev_uv,
                        w,
                        h,
                        rules.warp_diag_flipped,
                        rules.mv_min_len_px,
                        color,
                    );
                }
                if !verts.is_empty() {
                    let mut b = Batch::default();
                    b.segments(
                        self.stroke_mode(),
                        LineRule::Diamond,
                        Blend::SrcAlpha,
                        w as f32,
                        h as f32,
                        &verts,
                    );
                    self.run_batch(
                        encoder,
                        &b,
                        &self.main[prev_i].view,
                        wgpu::LoadOp::Load,
                        None,
                        None,
                        "motion vectors",
                    );
                }
                self.scratch.verts = verts;
            }
        }

        // ---- 2. warp pass --------------------------------------------------------------
        let decay = if rules.decay_8bit {
            geom::d3d_color_component(input.post.decay)
        } else {
            input.post.decay.min(1.0)
        };
        let toff = self.texel_offset_uv();
        if let Some(bl) = input.md2_blend.as_ref() {
            let wrap = if input.post.wrap {
                Address::Wrap
            } else {
                Address::Clamp
            };
            self.run_md2_warp(
                encoder, input, bl, &warp_uv, decay, toff, wrap, prev_i, next_i,
            );
        } else {
            let sampler_wrap = if input.post.wrap {
                Address::Wrap
            } else {
                Address::Clamp
            };
            match &self.warp_shader {
                None => {
                    if let Some(wg) = gpu_warp {
                        let verts = geom::build_warp_gpu_vertices(&self.geom, &wg.vars);
                        let vu = self.warp_vs_uniforms(wg, toff, decay);
                        self.run_warp_gpu_plain(
                            encoder,
                            &verts,
                            &vu,
                            &self.main[prev_i].view,
                            self.pipes.sampler(Filter::Linear, sampler_wrap, false),
                            &self.main[next_i].view,
                        );
                    } else {
                        let mut verts = geom::build_warp_vertices(
                            &self.geom,
                            &warp_uv,
                            [decay, decay, decay, 1.0],
                            toff,
                            rules.ang_seam_fix,
                            rules.warp_diag_flipped,
                        );
                        geom::fix_nan_uv_triangles(&mut verts);
                        let mut b = Batch::default();
                        b.tex(Blend::Replace, &verts);
                        let bg = self.tex_bind_group(
                            &self.main[prev_i].view,
                            self.pipes.sampler(Filter::Linear, sampler_wrap, false),
                        );
                        self.run_batch(
                            encoder,
                            &b,
                            &self.main[next_i].view,
                            wgpu::LoadOp::Clear(BLACK),
                            Some(&bg),
                            None,
                            "warp (no shader)",
                        );
                    }
                }
                Some(pass) => {
                    let diffuse_scalar = if rules.shader_warp_diffuse_is_decay {
                        decay
                    } else {
                        1.0
                    };

                    let mut rng = Rng::new(self.cfg.seed).fork(input.frame as u64 * 4);
                    let mu = uniforms::fill_milk_uniforms(
                        &MilkUniformInputs::for_frame(
                            w,
                            h,
                            input,
                            &self.rot,
                            &rules,
                            input.warp_random.as_ref(),
                        ),
                        &mut rng,
                    );
                    let main_view = &self.main[prev_i].view;
                    let warp_wrap = self
                        .rules
                        .warp_main_sampler_from_wrap
                        .then_some(sampler_wrap);
                    if let Some(wg) = gpu_warp {
                        let verts = geom::build_warp_gpu_vertices(&self.geom, &wg.vars);
                        let vu = self.warp_vs_uniforms(wg, toff, diffuse_scalar);
                        self.run_shader_pass(
                            encoder,
                            pass,
                            &mu,
                            &input.ext,
                            main_view,
                            PassVerts::Warp(&verts, &vu),
                            &self.main[next_i].view,
                            "warp (shader)",
                            warp_wrap,
                        );
                    } else {
                        let mut verts = geom::build_warp_vertices(
                            &self.geom,
                            &warp_uv,
                            [diffuse_scalar, diffuse_scalar, diffuse_scalar, 1.0],
                            toff,
                            rules.ang_seam_fix,
                            rules.warp_diag_flipped,
                        );
                        geom::fix_nan_uv_triangles(&mut verts);
                        self.run_shader_pass(
                            encoder,
                            pass,
                            &mu,
                            &input.ext,
                            main_view,
                            PassVerts::Mesh(&verts),
                            &self.main[next_i].view,
                            "warp (shader)",
                            warp_wrap,
                        );
                    }
                }
            }
        }

        // ---- 3. snapshot after warp ----------------------------------------------------
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.main[next_i].texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &self.warped.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );

        // ---- 4. blur chain (from the previous image, like MD2 / projectM) ---------------
        // `native_blur`: a shader of the preset samples blur (projectM's `BlurTexture::m_blurLevel > 0`). The blur chain
        // may also run only because a diagnostic stage was requested; that never has side effects in projectM's own
        // flow, so everything below keys off `native_blur`, not `levels`.
        let native_blur = self.blur_levels_required();
        let levels = native_blur.max(self.diagnostic_blur_levels());
        self.last_blur_levels = native_blur;
        if levels > 0 {
            self.run_blur(encoder, input, &self.main[prev_i].view, levels);
        }

        // ---- 4b. MD3 sprites, layer 1 ("deepest"): into the feedback image after warp and blur -------
        self.draw_sprite_layer(encoder, input, 1, &self.main[next_i].view);

        // ---- 5. shapes, waves, main wave, darken center, borders on `next` -------------
        {
            let mut b = Batch::default();
            // projectM quirk (`BlurTexture::Update` runs `AllocateTextures` -> `Framebuffer::SetSize`, which leaves FBO 0
            // bound *before* its "remember previously bound framebuffer" snapshot, so on the call that allocates the blur
            // textures — the first frame after load/resize — it restores the default framebuffer): every overlay of that
            // frame goes to the wrong target and is lost. Only presets whose shaders sample blur natively are affected.
            let overlays_lost =
                rules.blur_alloc_drops_overlays && native_blur > 0 && self.frames_since_resize == 0;
            // `GetVertices` runs every frame, also when the overlays are lost (it advances the wave-buffer tail).
            let pm_wave = if rules.pm_waveforms {
                self.advance_main_wave_pm(input)
            } else {
                None
            };
            if !overlays_lost {
                let mut scr = std::mem::take(&mut self.scratch);
                self.build_overlay(&mut b, input, aspect, pm_wave, &mut scr);
                self.scratch = scr;
            }
            let sprite_split = input.sprites.iter().any(|s| matches!(s.layer, 2 | 3));
            if !b.is_empty() || sprite_split {
                let bg = self.tex_bind_group(
                    &self.main[prev_i].view,
                    self.pipes.sampler(
                        if rules.shape_main_nearest {
                            Filter::Point
                        } else {
                            Filter::Linear
                        },
                        Address::Wrap,
                        false,
                    ),
                );
                let leak = self.tex_bind_group(
                    &self.main[prev_i].view,
                    self.pipes.sampler(Filter::Linear, Address::Clamp, false),
                );
                self.run_overlay(encoder, input, &b, next_i, &bg, &leak);
            }
        }

        // ---- 5b. MD3 sprites, layer 4 ("after waveform") --------------------------------------------
        self.draw_sprite_layer(encoder, input, 4, &self.main[next_i].view);

        // ---- 6. composite --------------------------------------------------------------
        if let Some(bl) = input.md2_blend.as_ref() {
            self.run_md2_comp(encoder, input, bl, aspect, prev_i, next_i);
        } else {
            let comp_src = if rules.comp_reads_previous && self.comp_shader.is_some() {
                prev_i
            } else {
                next_i
            };
            match &self.comp_shader {
                Some(pass) => {
                    let shade = geom::hue_shade(
                        input.time as f32,
                        input.hue_offsets.unwrap_or(self.rot.hue_offsets),
                    );
                    let verts =
                        geom::build_comp_vertices(w, h, aspect, &shade, rules.comp_uv_half_texel);
                    let mut rng = Rng::new(self.cfg.seed).fork(input.frame as u64 * 4 + 1);
                    let mu = uniforms::fill_milk_uniforms(
                        &MilkUniformInputs::for_frame(
                            w,
                            h,
                            input,
                            &self.rot,
                            &rules,
                            input.comp_random.as_ref(),
                        ),
                        &mut rng,
                    );
                    let main_view = self.comp_source_view(&self.main[comp_src].view);
                    self.run_shader_pass(
                        encoder,
                        pass,
                        &mu,
                        &input.ext,
                        main_view,
                        PassVerts::Mesh(&verts),
                        &self.final_tex.view,
                        "comp (shader)",
                        None,
                    );
                }
                None => {
                    let b = self.build_noshader_comp(input);
                    let wrap = if self.rules.noshader_comp_wrap {
                        Address::Wrap
                    } else {
                        Address::Clamp
                    };
                    let src_view = self.comp_source_view(&self.main[next_i].view);
                    let bg = self
                        .tex_bind_group(src_view, self.pipes.sampler(Filter::Linear, wrap, false));
                    self.run_batch(
                        encoder,
                        &b,
                        &self.final_tex.view,
                        wgpu::LoadOp::Clear(BLACK),
                        Some(&bg),
                        None,
                        "comp (no shader)",
                    );
                }
            }
        }

        // ---- 6b. MD3 sprites, layer 0: on the composited image; burn also feeds the next frame --------
        self.draw_top_sprites(
            encoder,
            input,
            &self.final_tex.view,
            &self.main[next_i].view,
        );

        // ---- 7. blit to the host target ------------------------------------------------
        if let Some(target) = target {
            self.blit(encoder, target);
        }
        #[cfg(feature = "diagnostics")]
        {
            self.comp_source_override = None;
        }

        // keep this frame's UVs for the next frame's motion vectors, reusing the previous buffer
        if uv_unneeded {
            self.prev_warp_uv = None;
        } else {
            let mut kept = self.prev_warp_uv.take().unwrap_or_default();
            kept.clear();
            kept.extend_from_slice(&warp_uv);
            self.prev_warp_uv = Some(kept);
        }
        let gpu_src = gpu_warp;
        match (gpu_src, &mut self.prev_warp_gpu) {
            (Some(src), Some(dst)) => {
                dst.vars.clone_from(&src.vars);
                dst.anim = src.anim;
            }
            (Some(src), slot) => *slot = Some(src.clone()),
            (None, slot) => *slot = None,
        }
        // the next frame's motion vector pass reads this frame's GPU UVs: start them now if it is likely to
        if rules.warp_uv_on_gpu && mv_visible {
            self.submit_uv_readback();
        } else if let Some(rb) = &mut self.uv_readback {
            rb.pending = None;
        }
        self.main_idx = next_i;
        self.frames_since_resize += 1;
        Ok(())
    }
}
