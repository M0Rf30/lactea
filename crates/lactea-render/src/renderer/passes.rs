// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-FileCopyrightText: 2005-2013 Nullsoft, Inc.
// SPDX-License-Identifier: (MIT OR Apache-2.0) AND BSD-3-Clause

//! Pass execution: batches, user shader passes, MD2 warp / composite, blur, blit and sprites.

use super::*;

impl Renderer {
    /// Replace the FFT texture's contents ([`TexSource::Fft`]). Recorded as a buffer copy in `encoder` rather than
    /// `queue.write_texture`: a queue write lands at the next submit, ahead of everything already recorded, so two
    /// frames of one renderer recorded into one encoder would both see the last spectrum.
    pub(super) fn record_fft_upload(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        fft: &[f32; FFT_BINS],
    ) {
        let staging = buffer_init(
            &self.device,
            "fft",
            fft.as_slice(),
            wgpu::BufferUsages::COPY_SRC,
        );
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(FFT_BINS as u32 * 4),
                    rows_per_image: Some(1),
                },
            },
            wgpu::TexelCopyTextureInfo {
                texture: &self.fft.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: FFT_BINS as u32,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
    }

    /// [`record_fft_upload`](Self::record_fft_upload) for BeatDrop's [`crate::BeatDropTexture`]: the two rows of
    /// `sampler_fft` in one copy, the two rows of `sampler_wave` in one copy each (a row of `NUM_WAVEFORM_SAMPLES`
    /// floats is not a multiple of the 256-byte row pitch a two-row copy needs).
    pub(super) fn record_beatdrop_upload(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        data: &crate::BeatDropTexture,
    ) {
        let Some(wave) = &self.wave else {
            return;
        };
        let texels = data.texels();
        let staging = buffer_init(
            &self.device,
            "beatdrop",
            texels.as_slice(),
            wgpu::BufferUsages::COPY_SRC,
        );
        let copy = |encoder: &mut wgpu::CommandEncoder,
                    tex: &Tex,
                    first_texel: usize,
                    y: u32,
                    rows: u32| {
            encoder.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer: &staging,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: (first_texel * 4) as u64,
                        // the pitch of a copy of several rows must be a multiple of 256 bytes; one row has none
                        bytes_per_row: (rows > 1).then_some(tex.w * 4),
                        rows_per_image: (rows > 1).then_some(rows),
                    },
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &tex.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: 0, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: tex.w,
                    height: rows,
                    depth_or_array_layers: 1,
                },
            );
        };
        copy(encoder, &self.fft, 0, 0, 2);
        let wave_at = 2 * crate::BEATDROP_FFT_BINS;
        copy(encoder, wave, wave_at, 0, 1);
        copy(encoder, wave, wave_at + crate::BEATDROP_WAVE_SAMPLES, 1, 1);
    }

    /// BeatDrop's `sampler_wave` as the translator hands it over: the user texture `"wave"` under
    /// `CompatRules::md2_shader_inputs` (the frozen ABI has no [`TexSource`] for it, so it cannot be told from a
    /// host-supplied image by its source; BeatDrop binds `m_lpWaveTexture` to the root name `wave` before looking
    /// for an image, and so does this). `None` for every other source.
    pub(super) fn engine_wave(&self, src: &TexSource) -> Option<&Tex> {
        match src {
            TexSource::User(name) if self.rules.md2_shader_inputs && name == "wave" => {
                self.wave.as_ref()
            }
            _ => None,
        }
    }

    pub(super) fn tex_bind_group(
        &self,
        view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tex"),
            layout: &self.pipes.bgl_tex,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        })
    }

    /// Execute a fixed-function batch in one render pass.
    pub(super) fn run_batch(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        batch: &Batch,
        view: &wgpu::TextureView,
        load: wgpu::LoadOp<wgpu::Color>,
        tex_bg: Option<&wgpu::BindGroup>,
        leak_bg: Option<&wgpu::BindGroup>,
        label: &str,
    ) {
        self.run_batch_range(
            encoder,
            batch,
            0..batch.cmds.len(),
            view,
            load,
            tex_bg,
            leak_bg,
            label,
        );
    }

    /// [`Renderer::run_batch`] for the commands `cmds` only.
    pub(super) fn run_batch_range(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        batch: &Batch,
        cmds: std::ops::Range<usize>,
        view: &wgpu::TextureView,
        load: wgpu::LoadOp<wgpu::Color>,
        tex_bg: Option<&wgpu::BindGroup>,
        leak_bg: Option<&wgpu::BindGroup>,
        label: &str,
    ) {
        let wf_buf = buffer_init(&self.device, "wf", &batch.wf, wgpu::BufferUsages::VERTEX);
        let mesh_buf = buffer_init(
            &self.device,
            "mesh",
            &batch.mesh,
            wgpu::BufferUsages::VERTEX,
        );
        // `image=` textures: projectM's built-in noise textures by name, else the user texture (a 1x1 black
        // placeholder when the host had none, like projectM's `TextureManager` placeholder) with the sampler its
        // prefix asks for (no mip chain: filter only)
        let image_bgs: Vec<wgpu::BindGroup> = batch
            .images
            .iter()
            .map(|im| {
                let view = match noise::builtin_image_noise(&im.name) {
                    Some(n) => &self.noise[n.index()].view,
                    None => self
                        .user_tex
                        .get(&im.name)
                        .map_or(&self.dummy_2d.view, |u| u.view_at(self.anim_time)),
                };
                let filter = if im.linear {
                    Filter::Linear
                } else {
                    Filter::Point
                };
                let address = if im.wrap {
                    Address::Wrap
                } else {
                    Address::Clamp
                };
                self.tex_bind_group(view, self.pipes.sampler(filter, address, true))
            })
            .collect();
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        for &(kind, blend, prim, start, count) in &batch.cmds[cmds] {
            match kind {
                Kind::Flat => {
                    pass.set_pipeline(&self.pipes.flat[&(blend, prim, self.rules.main_alpha)]);
                    pass.set_vertex_buffer(0, wf_buf.slice(..));
                }
                // every caller binds the groups its batch kinds need; a draw without one is skipped
                Kind::Tex => {
                    let Some(bg) = tex_bg else {
                        continue;
                    };
                    pass.set_pipeline(&self.pipes.tex[&(blend, self.rules.main_alpha)]);
                    pass.set_bind_group(0, bg, &[]);
                    pass.set_vertex_buffer(0, mesh_buf.slice(..));
                }
                Kind::TexLeak => {
                    let Some(bg) = leak_bg else {
                        continue;
                    };
                    pass.set_pipeline(&self.pipes.tex[&(blend, self.rules.main_alpha)]);
                    pass.set_bind_group(0, bg, &[]);
                    pass.set_vertex_buffer(0, mesh_buf.slice(..));
                }
                Kind::TexImage(i) => {
                    pass.set_pipeline(&self.pipes.tex[&(blend, self.rules.main_alpha)]);
                    pass.set_bind_group(0, &image_bgs[i as usize], &[]);
                    pass.set_vertex_buffer(0, mesh_buf.slice(..));
                }
            }
            pass.draw(start..start + count, 0..1);
        }
    }

    pub(super) fn texsize_of(
        &self,
        src: &TexSource,
        user_tex: &HashMap<String, UserTex>,
    ) -> [f32; 4] {
        let (w, h) = match src {
            TexSource::Main => (self.cfg.width, self.cfg.height),
            TexSource::Blur(k) if (1..=3).contains(k) => {
                let t = &self.blur[*k as usize * 2 - 1];
                (t.w, t.h)
            }
            TexSource::Blur(_) => (1, 1),
            TexSource::Noise(n) => {
                let t = &self.noise[n.index()];
                (t.w, t.h)
            }
            TexSource::User(name) => match self.engine_wave(src) {
                Some(w) => (w.w, w.h),
                None => match user_tex.get(&name.to_ascii_lowercase()) {
                    Some(u) => (u.tex.w, u.tex.h),
                    None => (1, 1),
                },
            },
            TexSource::Fft => (self.fft.w, self.fft.h),
        };
        [w as f32, h as f32, 1.0 / w as f32, 1.0 / h as f32]
    }

    /// Uniforms of the GPU warp vertex stage ([`shaders::warp_vs`]).
    pub(super) fn warp_vs_uniforms(
        &self,
        wg: &crate::frame::WarpGpu,
        toff: [f32; 2],
        diffuse: f32,
    ) -> WarpVsUniforms {
        let (ax, ay) = (self.geom.aspect_x, self.geom.aspect_y);
        WarpVsUniforms {
            aspect: [ax, ay, 1.0 / ax, 1.0 / ay],
            timing: [wg.anim.warp_time, wg.anim.scale_inv, toff[0], toff[1]],
            factors: wg.anim.f,
            diffuse: [diffuse, diffuse, diffuse, 1.0],
        }
    }

    pub(super) fn warp_vs_bind_group(&self, vu: &WarpVsUniforms) -> wgpu::BindGroup {
        let buf = buffer_init(
            &self.device,
            "warp_vs",
            std::slice::from_ref(vu),
            wgpu::BufferUsages::UNIFORM,
        );
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("warp_vs"),
            layout: &self.pipes.bgl_warp_vs,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buf.as_entire_binding(),
            }],
        })
    }

    /// `WarpedBlit_NoShaders` with the GPU vertex stage: `next = prev(uv) * decay`.
    pub(super) fn run_warp_gpu_plain(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        verts: &[WarpVertex],
        vu: &WarpVsUniforms,
        src: &wgpu::TextureView,
        samp: &wgpu::Sampler,
        target: &wgpu::TextureView,
    ) {
        let tex_bg = self.tex_bind_group(src, samp);
        let vs_bg = self.warp_vs_bind_group(vu);
        let vb = buffer_init(
            &self.device,
            "warp verts",
            verts,
            wgpu::BufferUsages::VERTEX,
        );
        let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("warp (no shader, gpu uv)"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        rp.set_pipeline(&self.pipes.warp_tex[&self.rules.main_alpha]);
        rp.set_bind_group(0, &tex_bg, &[]);
        rp.set_bind_group(1, &vs_bg, &[]);
        rp.set_vertex_buffer(0, vb.slice(..));
        if let Some((ib, n)) = &self.warp_indices {
            rp.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
            rp.draw_indexed(0..*n, 0, 0..1);
        }
    }

    /// Draw a user warp / composite shader over `verts` into `target`.
    pub(super) fn run_shader_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pass: &ShaderPass,
        mu: &MilkUniforms,
        ext: &ShaderExt,
        main_view: &wgpu::TextureView,
        verts: PassVerts<'_>,
        target: &wgpu::TextureView,
        label: &str,
        warp_wrap: Option<Address>,
    ) {
        self.run_shader_pass_ex(
            encoder,
            pass,
            &self.user_tex,
            PassBlend::Replace,
            mu,
            ext,
            main_view,
            verts,
            target,
            label,
            warp_wrap,
        );
    }

    /// [`Renderer::run_shader_pass`] with the user textures of the pass's own preset (`m_OldShaders` keeps the old
    /// preset's) and MD2's blend-pass output modes: `Replace` clears the target first, the alpha modes keep it and
    /// blend the shader's output by the vertex alpha.
    pub(super) fn run_shader_pass_ex(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pass: &ShaderPass,
        user_tex: &HashMap<String, UserTex>,
        blend: PassBlend,
        mu: &MilkUniforms,
        ext: &ShaderExt,
        main_view: &wgpu::TextureView,
        verts: PassVerts<'_>,
        target: &wgpu::TextureView,
        label: &str,
        warp_wrap: Option<Address>,
    ) {
        let mu_buf = buffer_init(
            &self.device,
            "mu",
            std::slice::from_ref(mu),
            wgpu::BufferUsages::UNIFORM,
        );
        let mx_buf = buffer_init(
            &self.device,
            "mx",
            std::slice::from_ref(&ext.uniforms()),
            wgpu::BufferUsages::UNIFORM,
        );
        let bg0 = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mu"),
            layout: &self.pipes.bgl_mu,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: mu_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: BINDING_MILK_EXT,
                    resource: mx_buf.as_entire_binding(),
                },
            ],
        });

        let mut pu = PassUniforms::default();
        for (i, src) in pass
            .interface
            .texsize_slots
            .iter()
            .enumerate()
            .take(lactea_abi::MAX_PASS_TEXTURES)
        {
            pu.texsize[i] = self.texsize_of(src, user_tex);
        }
        let pu_buf = buffer_init(
            &self.device,
            "pu",
            std::slice::from_ref(&pu),
            wgpu::BufferUsages::UNIFORM,
        );

        let mut entries: Vec<wgpu::BindGroupEntry> = vec![wgpu::BindGroupEntry {
            binding: lactea_abi::BINDING_PASS_UNIFORMS,
            resource: pu_buf.as_entire_binding(),
        }];
        // projectM `WarpedBlit`: the per-frame `wrap` sampler replaces the one of the first `main` descriptor.
        let wrap_slot = warp_wrap.and_then(|_| {
            let name_of = |t: &lactea_abi::TextureBinding| {
                t.hlsl_name
                    .strip_prefix("sampler_")
                    .unwrap_or(&t.hlsl_name)
                    .to_owned()
            };
            let first = pass
                .interface
                .textures
                .iter()
                .enumerate()
                .filter(|(_, t)| matches!(t.source, TexSource::Main))
                .min_by(|a, b| name_of(a.1).cmp(&name_of(b.1)))?;
            // `main` is always in the set: if it sorts first and the shader does not use it, no bound sampler changes
            (name_of(first.1).as_str() <= "main").then_some(first.0)
        });
        for (i, t) in pass.interface.textures.iter().enumerate() {
            let want3d = t.dim == TexDim::D3;
            let view: &wgpu::TextureView = match &t.source {
                TexSource::Main => main_view,
                TexSource::Blur(k) if (1..=3).contains(k) => &self.blur[*k as usize * 2 - 1].view,
                TexSource::Blur(_) => &self.dummy_2d.view,
                TexSource::Noise(n) => &self.noise[n.index()].view,
                TexSource::User(name) => match self.engine_wave(&t.source) {
                    Some(w) => &w.view,
                    None => match user_tex.get(&name.to_ascii_lowercase()) {
                        Some(u) => u.view_at(self.anim_time),
                        None => &self.dummy_2d.view,
                    },
                },
                TexSource::Fft => &self.fft.view,
            };
            // the bound view must match the declared dimensionality
            let is3d = matches!(
                &t.source,
                TexSource::Noise(NoiseTex::VolLq | NoiseTex::VolHq)
            );
            let view = if want3d == is3d {
                view
            } else if want3d {
                &self.dummy_3d.view
            } else {
                &self.dummy_2d.view
            };
            entries.push(wgpu::BindGroupEntry {
                binding: lactea_abi::texture_binding(i as u32),
                resource: wgpu::BindingResource::TextureView(view),
            });
            let (filter, address) = match (wrap_slot == Some(i), warp_wrap) {
                (true, Some(a)) => (Filter::Linear, a),
                _ => (t.filter, t.address),
            };
            entries.push(wgpu::BindGroupEntry {
                binding: lactea_abi::sampler_binding(i as u32),
                resource: wgpu::BindingResource::Sampler(self.pipes.sampler(
                    filter,
                    address,
                    self.rules.no_mipmaps,
                )),
            });
        }
        let bg1 = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pass"),
            layout: &pass.pass_bgl,
            entries: &entries,
        });

        let (vb, count, vs_bg) = match &verts {
            PassVerts::Mesh(v) => (
                buffer_init(&self.device, "verts", v, wgpu::BufferUsages::VERTEX),
                v.len(),
                None,
            ),
            PassVerts::Warp(v, vu) => (
                buffer_init(&self.device, "warp verts", v, wgpu::BufferUsages::VERTEX),
                v.len(),
                Some(self.warp_vs_bind_group(vu)),
            ),
        };
        let sink = pass.uv_sink.then_some(wgpu::RenderPassColorAttachment {
            view: &self.warp_sink.view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(BLACK),
                store: wgpu::StoreOp::Store,
            },
        });
        let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[
                Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: match blend {
                            PassBlend::Replace => wgpu::LoadOp::Clear(BLACK),
                            _ => wgpu::LoadOp::Load,
                        },
                        store: wgpu::StoreOp::Store,
                    },
                }),
                sink,
            ][..if pass.uv_sink { 2 } else { 1 }],
            ..Default::default()
        });
        let alpha_pipeline = match (blend, pass.alpha_pipelines.as_ref()) {
            (PassBlend::SrcAlpha, Some(a)) => Some(&a[0]),
            (PassBlend::InvSrcAlpha, Some(a)) => Some(&a[1]),
            (PassBlend::Replace, _) => Some(&pass.pipeline),
            _ => None,
        };
        let pipeline = match &vs_bg {
            Some(_) => pass.warp_pipeline.as_ref(),
            None => alpha_pipeline,
        };
        // `render_frame` rejects the inputs that lack a pipeline (a blend without `md2_preset_blend`); a
        // warp-vertex pass is only requested for warp shader passes.
        let Some(pipeline) = pipeline else {
            return;
        };
        rp.set_pipeline(pipeline);
        if let Some(bg) = &vs_bg {
            rp.set_bind_group(2, bg, &[]);
        }
        rp.set_bind_group(0, &bg0, &[]);
        rp.set_bind_group(1, &bg1, &[]);
        rp.set_vertex_buffer(0, vb.slice(..));
        match (&verts, &self.warp_indices) {
            (PassVerts::Mesh(_), _) => rp.draw(0..count as u32, 0..1),
            (PassVerts::Warp(..), Some((ib, n))) => {
                rp.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                rp.draw_indexed(0..*n, 0, 0..1);
            }
            // only projectM's rules request a warp-vertex pass, and they always have the index buffer
            (PassVerts::Warp(..), None) => {}
        }
    }

    /// The warp pass of `RenderFrame` while MD2 blends two presets: `WarpedBlit_Shaders` / `WarpedBlit_NoShaders` once
    /// or twice depending on which of the two presets use a warp shader ([`blend::warp_passes`]), all on the *blended*
    /// warp mesh (`warp_uv`, per-vertex alpha `bl.vert_alpha`). Pass 0 is opaque (old shader / old state), pass 1
    /// is alpha-blended over it (new shader); tiles whose alphas make a pass invisible are not drawn.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn run_md2_warp(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        input: &FrameInput,
        bl: &blend::Md2Blend,
        warp_uv: &[[f32; 2]],
        decay: f32,
        toff: [f32; 2],
        sampler_wrap: Address,
        prev_i: usize,
        next_i: usize,
    ) {
        let rules = self.rules;
        let (w, h) = (self.cfg.width, self.cfg.height);
        let old = self.old_shaders.as_ref();
        let passes = blend::warp_passes(
            old.is_some_and(|o| o.warp.is_some()),
            self.warp_shader.is_some(),
        );
        let src = &self.main[prev_i].view;
        let target = &self.main[next_i].view;
        for dp in passes {
            let cull = dp.cull_tiles.then_some(dp.flip_culling);
            let mode = match (dp.alpha_blend, dp.flip_alpha) {
                (false, _) => PassBlend::Replace,
                (true, false) => PassBlend::SrcAlpha,
                (true, true) => PassBlend::InvSrcAlpha,
            };
            if dp.shaders {
                let (pass, user_tex, q, blur, ext, rand) = match dp.which {
                    Which::New => {
                        let Some(pass) = self.warp_shader.as_ref() else {
                            continue;
                        };
                        (
                            pass,
                            &self.user_tex,
                            &input.q,
                            &input.blur,
                            &input.ext,
                            input.warp_random.as_ref(),
                        )
                    }
                    Which::Old => {
                        let Some((o, pass)) = old.and_then(|o| Some((o, o.warp.as_ref()?))) else {
                            continue;
                        };
                        (
                            pass,
                            &o.user_tex,
                            &bl.old.q,
                            &bl.old.blur,
                            &bl.old.ext,
                            bl.old.warp_random.as_ref(),
                        )
                    }
                };
                let diffuse = if rules.shader_warp_diffuse_is_decay {
                    decay
                } else {
                    1.0
                };
                let mut rng = Rng::new(self.cfg.seed).fork(input.frame as u64 * 4);
                let mu = uniforms::fill_milk_uniforms(
                    &MilkUniformInputs {
                        q,
                        blur,
                        ext,
                        ..MilkUniformInputs::for_frame(w, h, input, &self.rot, &rules, rand)
                    },
                    &mut rng,
                );
                let mut verts = geom::build_warp_vertices_blend(
                    &self.geom,
                    warp_uv,
                    [diffuse, diffuse, diffuse],
                    &bl.vert_alpha,
                    toff,
                    rules.ang_seam_fix,
                    cull,
                );
                geom::fix_nan_uv_triangles(&mut verts);
                let warp_wrap = rules.warp_main_sampler_from_wrap.then_some(sampler_wrap);
                self.run_shader_pass_ex(
                    encoder,
                    pass,
                    user_tex,
                    mode,
                    &mu,
                    ext,
                    src,
                    PassVerts::Mesh(&verts),
                    target,
                    "warp (shader, blend)",
                    warp_wrap,
                );
            } else {
                let mut verts = geom::build_warp_vertices_blend(
                    &self.geom,
                    warp_uv,
                    [decay, decay, decay],
                    &bl.vert_alpha,
                    toff,
                    rules.ang_seam_fix,
                    cull,
                );
                geom::fix_nan_uv_triangles(&mut verts);
                let mut b = Batch::default();
                b.tex(
                    match mode {
                        PassBlend::Replace => Blend::Replace,
                        PassBlend::SrcAlpha => Blend::SrcAlpha,
                        PassBlend::InvSrcAlpha => Blend::InvSrcAlpha,
                    },
                    &verts,
                );
                let bg = self
                    .tex_bind_group(src, self.pipes.sampler(Filter::Linear, sampler_wrap, false));
                self.run_batch(
                    encoder,
                    &b,
                    target,
                    if mode == PassBlend::Replace {
                        wgpu::LoadOp::Clear(BLACK)
                    } else {
                        wgpu::LoadOp::Load
                    },
                    Some(&bg),
                    None,
                    "warp (no shader, blend)",
                );
            }
        }
    }

    /// The composite of `RenderFrame` while MD2 blends two presets: `ShowToUser_NoShaders` (whole quad, from the
    /// blended per-frame values) and / or `ShowToUser_Shaders` with the old and the new composite shader on the 32x24
    /// composite mesh, whose vertex alphas are looked up in the warp mesh's ([`blend::comp_passes`],
    /// [`blend::comp_vertex_alpha`]).
    pub(super) fn run_md2_comp(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        input: &FrameInput,
        bl: &blend::Md2Blend,
        aspect: (f32, f32),
        prev_i: usize,
        next_i: usize,
    ) {
        let rules = self.rules;
        let (w, h) = (self.cfg.width, self.cfg.height);
        let old = self.old_shaders.as_ref();
        let passes = blend::comp_passes(
            old.is_some_and(|o| o.comp.is_some()),
            self.comp_shader.is_some(),
        );
        let shade = geom::hue_shade(
            input.time as f32,
            input.hue_offsets.unwrap_or(self.rot.hue_offsets),
        );
        let mut grid = geom::comp_grid(w, h, aspect, &shade, rules.comp_uv_half_texel);
        let mut alpha = vec![255u8; grid.len()];
        for j in 0..geom::FCGSY {
            for i in 0..geom::FCGSX {
                let (x01, y01) = blend::comp_vertex_xy01(i, j, w, h);
                let a = blend::comp_vertex_alpha(
                    &bl.vert_alpha,
                    [self.geom.gx, self.geom.gy],
                    x01,
                    y01,
                );
                alpha[i + j * geom::FCGSX] = a;
                grid[i + j * geom::FCGSX].diffuse[3] = f32::from(a) / 255.0;
            }
        }
        let idx = geom::comp_indices();
        for dp in passes {
            if dp.shaders {
                let mut verts = Vec::with_capacity(idx.len());
                for t in idx.as_chunks::<3>().0 {
                    if !dp.cull_tiles
                        || blend::tile_needed(
                            [alpha[t[0]], alpha[t[1]], alpha[t[2]]],
                            dp.flip_culling,
                        )
                    {
                        verts.extend(t.iter().map(|&k| grid[k]));
                    }
                }
                let mode = match (dp.alpha_blend, dp.flip_alpha) {
                    (false, _) => PassBlend::Replace,
                    (true, false) => PassBlend::SrcAlpha,
                    (true, true) => PassBlend::InvSrcAlpha,
                };
                let (pass, user_tex, q, blur, ext, rand) = match dp.which {
                    Which::New => {
                        let Some(pass) = self.comp_shader.as_ref() else {
                            continue;
                        };
                        (
                            pass,
                            &self.user_tex,
                            &input.q,
                            &input.blur,
                            &input.ext,
                            input.comp_random.as_ref(),
                        )
                    }
                    Which::Old => {
                        let Some((o, pass)) = old.and_then(|o| Some((o, o.comp.as_ref()?))) else {
                            continue;
                        };
                        (
                            pass,
                            &o.user_tex,
                            &bl.old.q,
                            &bl.old.blur,
                            &bl.old.ext,
                            bl.old.comp_random.as_ref(),
                        )
                    }
                };
                let mut rng = Rng::new(self.cfg.seed).fork(input.frame as u64 * 4 + 1);
                let mu = uniforms::fill_milk_uniforms(
                    &MilkUniformInputs {
                        q,
                        blur,
                        ext,
                        ..MilkUniformInputs::for_frame(w, h, input, &self.rot, &rules, rand)
                    },
                    &mut rng,
                );
                let comp_src = if rules.comp_reads_previous {
                    prev_i
                } else {
                    next_i
                };
                let main_view = self.comp_source_view(&self.main[comp_src].view);
                self.run_shader_pass_ex(
                    encoder,
                    pass,
                    user_tex,
                    mode,
                    &mu,
                    ext,
                    main_view,
                    PassVerts::Mesh(&verts),
                    &self.final_tex.view,
                    "comp (shader, blend)",
                    None,
                );
            } else {
                let b = self.build_noshader_comp(input);
                let wrap = if self.rules.noshader_comp_wrap {
                    Address::Wrap
                } else {
                    Address::Clamp
                };
                let src_view = self.comp_source_view(&self.main[next_i].view);
                let bg =
                    self.tex_bind_group(src_view, self.pipes.sampler(Filter::Linear, wrap, false));
                self.run_batch(
                    encoder,
                    &b,
                    &self.final_tex.view,
                    wgpu::LoadOp::Clear(BLACK),
                    Some(&bg),
                    None,
                    "comp (no shader, blend)",
                );
            }
        }
    }

    /// `BlurPasses`.
    pub(super) fn run_blur(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        input: &FrameInput,
        source: &wgpu::TextureView,
        levels: u8,
    ) {
        let passes = blur::pass_count(levels);
        let (bmin, bmax) = blur::safe_blur_min_max(input.blur.min, input.blur.max);
        let (scale, bias) = blur::scale_bias(bmin, bmax);
        let ht = blur::horizontal_taps();
        let vt = blur::vertical_taps();
        let edge_darken = input.blur.edge_darken;
        let clamp_linear = self.pipes.sampler(Filter::Linear, Address::Clamp, false);
        for i in 0..passes {
            let (src_view, sw, sh) = if i == 0 {
                (source, self.cfg.width as f32, self.cfg.height as f32)
            } else {
                (
                    &self.blur[i - 1].view,
                    self.blur[i - 1].w as f32,
                    self.blur[i - 1].h as f32,
                )
            };
            let c0 = [sw, sh, 1.0 / sw, 1.0 / sh];
            let mut p = [[0.0f32; 4]; 6]; // c0, c1, c2, c3, c5, c6
            p[0] = c0;
            if i % 2 == 0 {
                p[1] = ht.w;
                p[2] = ht.d;
                // projectM samples the (upside-down stored) source of the first pass with
                // `uv.y` flipped, so its "+1 texel" bias moves the other way (c3.w).
                let vsign = if i == 0 && self.rules.blur_src_flipped {
                    -1.0
                } else {
                    1.0
                };
                p[3] = [scale[i / 2], bias[i / 2], ht.w_div, vsign];
            } else {
                p[4] = [vt.w[0], vt.w[1], vt.d[0], vt.d[1]];
                p[5] = if i == 1 {
                    [vt.w_div, 1.0 - edge_darken, edge_darken, 5.0]
                } else {
                    [vt.w_div, 1.0, 0.0, 5.0]
                };
            }
            let make = || {
                let ub = buffer_init(
                    &self.device,
                    "blur params",
                    std::slice::from_ref(&p),
                    wgpu::BufferUsages::UNIFORM,
                );
                self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("blur"),
                    layout: &self.pipes.bgl_blur,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: ub.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(src_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(clamp_linear),
                        },
                    ],
                })
            };
            // Pass 0 reads the caller's source image; every later pass reads `blur[i - 1]`, so its bind group
            // only changes with the uniform values (constant unless the preset's blur settings move).
            let bg = if i == 0 {
                make()
            } else {
                let mut cache = self.blur_bg.borrow_mut();
                if cache.len() <= i {
                    cache.resize(i + 1, None);
                }
                match &cache[i] {
                    Some((key, bg)) if *key == p => bg.clone(),
                    _ => {
                        let bg = make();
                        cache[i] = Some((p, bg.clone()));
                        bg
                    }
                }
            };
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blur"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.blur[i].view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            rp.set_pipeline(if i % 2 == 0 {
                &self.pipes.blur_h
            } else {
                &self.pipes.blur_v
            });
            rp.set_bind_group(0, &bg, &[]);
            rp.draw(0..3, 0..1);
        }
    }

    /// The blit's uniform buffer and bind group depend only on the sizes, the output format and `final_tex`, so they
    /// are built once and dropped when `final_tex` is replaced (`resize`).
    pub(super) fn blit(&self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        let mut cache = self.blit_bg.borrow_mut();
        let bg = cache.get_or_insert_with(|| {
            let (w, h) = (self.cfg.width as f32, self.cfg.height as f32);
            let srgb = if self.cfg.output_format.is_srgb() {
                1.0
            } else {
                0.0
            };
            let params: [[f32; 4]; 2] = [[w, h, w, h], [srgb, 0.0, 0.0, 0.0]];
            let ub = buffer_init(
                &self.device,
                "blit",
                std::slice::from_ref(&params),
                wgpu::BufferUsages::UNIFORM,
            );
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("blit"),
                layout: &self.pipes.bgl_blit,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: ub.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&self.final_tex.view),
                    },
                ],
            })
        });
        let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("blit"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        rp.set_pipeline(&self.pipes.blit);
        rp.set_bind_group(0, &*bg, &[]);
        rp.draw(0..3, 0..1);
    }

    // ---- MD3 sprites -------------------------------------------------------------------------

    pub(super) fn sprite_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        draws: &[&SpriteDraw],
    ) {
        self.pipes.sprites(&self.device).draw(
            &self.device,
            encoder,
            view,
            &self.pipes.bgl_tex,
            self.pipes.sampler(Filter::Linear, Address::Wrap, false),
            &self.sprite_images,
            draws,
        );
    }

    /// The sprites of `layer` (1..=4) onto `view`, one after the other in the order of `FrameInput::sprites`. With
    /// `CompatRules::md3_sprite_burn_twice` a burning sprite is drawn a second time right after the first (MD3 burns
    /// into the very image it draws on).
    pub(super) fn draw_sprite_layer(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        input: &FrameInput,
        layer: u8,
        view: &wgpu::TextureView,
    ) {
        let mut draws: Vec<&SpriteDraw> = Vec::new();
        for s in input.sprites.iter().filter(|s| s.layer == layer) {
            draws.push(s);
            if s.burn && self.rules.md3_sprite_burn_twice {
                draws.push(s);
            }
        }
        if !draws.is_empty() {
            self.sprite_pass(encoder, view, &draws);
        }
    }

    /// Layer 0: the sprites onto the composited image; those that burn are also drawn into the feedback image,
    /// which the composite has already read.
    pub(super) fn draw_top_sprites(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        input: &FrameInput,
        final_view: &wgpu::TextureView,
        feedback_view: &wgpu::TextureView,
    ) {
        let draws: Vec<&SpriteDraw> = input.sprites.iter().filter(|s| s.layer == 0).collect();
        if draws.is_empty() {
            return;
        }
        self.sprite_pass(encoder, final_view, &draws);
        let burn: Vec<&SpriteDraw> = draws.iter().copied().filter(|s| s.burn).collect();
        if !burn.is_empty() {
            self.sprite_pass(encoder, feedback_view, &burn);
        }
    }

    /// Shapes, waves, main wave and borders into the feedback image `main[next_i]`. Sprite layers 2 (after the
    /// shapes) and 3 (after the waves) split the batch into several passes; without them it is one pass.
    pub(super) fn run_overlay(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        input: &FrameInput,
        b: &Batch,
        next_i: usize,
        bg: &wgpu::BindGroup,
        leak: &wgpu::BindGroup,
    ) {
        const LABEL: &str = "shapes+waves+borders";
        let view = &self.main[next_i].view;
        if !input.sprites.iter().any(|s| matches!(s.layer, 2 | 3)) {
            self.run_batch(
                encoder,
                b,
                view,
                wgpu::LoadOp::Load,
                Some(bg),
                Some(leak),
                LABEL,
            );
            return;
        }
        let end = b.cmds.len();
        for (range, layer) in [
            (0..b.marks[0], Some(2)),
            (b.marks[0]..b.marks[1], Some(3)),
            (b.marks[1]..end, None),
        ] {
            if !range.is_empty() {
                self.run_batch_range(
                    encoder,
                    b,
                    range,
                    view,
                    wgpu::LoadOp::Load,
                    Some(bg),
                    Some(leak),
                    LABEL,
                );
            }
            if let Some(layer) = layer {
                self.draw_sprite_layer(encoder, input, layer, view);
            }
        }
    }
}
