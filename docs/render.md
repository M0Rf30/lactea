# lactea-render notes

Port of MD2 `RenderFrame` (milkdropfs.cpp) to wgpu 30. See the crate docs (`src/lib.rs`) for the
pass order and `src/config.rs` for the full MD2-vs-projectM table (`Compat`, default projectM).

## Conventions
* Internal textures `Rgba8Unorm`, RGB-only writes (alpha stays 1 like MD2's X8R8G8B8).
* Clip space x right / y up; texture row 0 = top; warp mesh row 0 is stored at clip y = -1
  (bottom) and samples `v ~ 1`, exactly like `m_verts`.
* Everything MD2 draws through its y-flipped ortho projection (waves, shapes, mv, borders) is
  generated here directly in clip space (`y = 1` top): main wave y = `DrawWave` y (it negates
  at the end), custom wave / shape y = `2*y-1`.
* Lines / points: see "Strokes" below. `Compat::MilkDrop2` draws D3D9 thin quads (`geom::push_line` /
  `push_point`); `Compat::ProjectM` (default) emulates the oracle's GL rasterisation exactly.
* Per-frame buffers: the geometry builders (custom waves, main wave, overlay, motion vectors, borders, darken centre) fill a
  reusable `FrameScratch`, so a warm frame allocates nothing there. The vertex / uniform buffers handed to the GPU are still fresh per
  frame, because `render_frame` records into a host-owned encoder: two frames recorded into one encoder would share a
  `queue.write_buffer` slot and see the last write. Two cached exceptions, both dropped on `resize`: the blit uniform + bind group
  (`blit_bg`) and the uniform + bind group of blur passes 1.. (`blur_bg`, keyed by pass index and the exact uniform values; pass 0 reads a
  different source view every frame and is not cached). The encoder must be submitted before `read_stage`.
* No-shader composite hue tint (`CompatRules::shade_top_first`, `echo_diag_flipped`): MD2's `v3[0..1]` have `y = +1 + 1/H` (the "+H" side), which
  `support.cpp:179`'s `D3DXMatrixOrthoLH(2, -2)` puts at the **bottom** of the image, so `shade[0..1]` sit at the bottom corners and the
  echo quad's diagonal is not flipped (`false` for MD2); projectM has both `true`. The flat white filter quads have projectM's split too but
  show no gradient.

## Module layout (`src/renderer/`)

| file | holds |
|---|---|
| `mod.rs` | `Renderer` (resources, size / mesh checks, user textures, sprite images, FFT texture), the public API (`new`, `resize`, `set_mesh_size`, `render_frame[_to_output]`, `read_stage`, sprite calls), `readback_rgba8`, `Blend` (`Blend::ALL`), `FrameScratch`, the `Pipelines` struct |
| `pipelines.rs` | pipeline, layout and sampler construction: the `tex` / `flat` pipeline maps are keyed over every `Blend::ALL` mode (`blend_state` is the exhaustive match, so a new mode cannot miss its pipeline), sprite pipelines per output format, `with_uv_output` |
| `uv_readback.rs` | GPU warp UV readback (persistent buffers, own small submission, collected by the next frame's motion-vector pass) |
| `orchestrate.rs` | `render_frame_inner`: the pass sequence of one frame |
| `passes.rs` | pass execution: fixed-function batches, user shader passes, MD2 warp / composite, blur chain, blit, sprite passes, FFT uploads |
| `fixed.rs` | fixed-function geometry builders: overlay (shapes, custom waves), main wave, no-shader composite |
| `burn.rs` | `ProjectM::BurnInTexture`: one textured quad copied into the image the next warp samples (`Renderer::burn_texture`) |

## Size and mesh limits

`Renderer::new`, `resize` and `set_mesh_size` return `RenderError::Invalid` for a zero size, a size above the device's
`max_texture_dimension_2d`, a mesh above `warp_mesh::MAX_MESH_CELLS` (300 cells per axis) and a mesh that is not an even grid of
at least 2x2 cells. Readback and byte-length arithmetic is `usize` / `u64`. A refused `resize` / `set_mesh_size` leaves the renderer usable
at its old size. Test: `tests/limits.rs`. `lactea_render::readback_rgba8(device, queue, texture, w, h)` (the readback `Renderer::read_stage`
uses; submits its own copy, so the frame's command buffers must already be submitted) is public for hosts and for `lactea-testutil`.

## Features and backends (`Cargo.toml`)

* The workspace pulls wgpu with `default-features = false, features = ["std", "parking_lot", "wgsl"]`; `lactea-render` adds the backend
  per target: `vulkan` on Windows and non-Apple unix, `dx12` on Windows, `metal` on Apple, `webgpu` on wasm32.
* `gles` (off by default) = `wgpu/gles`: the OpenGL ES / WebGL fallback where Vulkan / Metal / DX12 are missing.
* `diagnostics` (off by default): image injection and extra read-backs for reference comparisons (the integration tests enable it).

## Noise and shader time

* `noise::gen_noise_vol(rng, size, zoom, &CompatRules)` (and `noise::generate`): with `CompatRules::noisevol_row_stride` (projectM) the cubic
  smoothing steps through z with a stride of `size`, not MD2's `size * size` (`MilkdropNoise.cpp:199,225,252-261` vs `plugin.cpp:3031-3072`), so
  the passes mix and overwrite unrelated rows and the volume differs from MD2's. Test: `noisevol_row_stride_follows_projectm_formula`.
* Shader time `_c2.x` is wrapped modulo 10000 (`t - (int)(t / 10000) * 10000`) when `CompatRules::shader_time_wrap` (projectM
  `MilkdropShader.cpp:168`, MilkDrop 3); MD2 passes the raw time (`milkdropfs.cpp:5289`).

## Sprites (`sprite.rs`, `Renderer`)

`has_sprite_image(id)` / `remove_sprite_image(id)` query and forget an image registered under `id`. `draw_sprites(encoder, target, draws)` draws any
layer onto an `Rgba8Unorm` view. `burn_sprites(encoder, draws)` draws only the `burn` ones into the image the next warp reads (call after
`render_frame_to_output`). `draw_sprites_to_output(encoder, target, draws)` draws onto a view of `RenderConfig::output_format` (loaded,
not cleared; its own sprite pipelines when that format is not `Rgba8Unorm`), so the engine can put user sprites over a soft cut's blend and still burn them
into both presets' feedback.

## Blur / source choices (identical in MD2 and projectM)
* blur chain input is the *previous* image (MD2 `m_lpVS[0]` / projectM previous framebuffer), after
  motion vectors were drawn onto it; the comp shader therefore sees blur one frame old.
* blur levels computed = max(`max_blur_level` of warp, comp) of the *current* frame (MD2 lags comp
  usage by one frame because of `m_nHighestBlurTexUsedThisFrame`).
* `GetSafeBlurMinMax` typo (`max = avg - 0.05`) kept.

## Strokes (1-px lines and points): `StrokeRaster`, `stroke.rs`

projectM draws `GL_LINES` (motion vectors), `GL_LINE_STRIP` (custom / main waves), `GL_LINE_LOOP` (shape outlines)
and `GL_POINTS` (dots), width 1. On the oracle's GPU (Mesa iris) `GL_LINE_SMOOTH` has **no antialiasing effect**
(pure 0/255 pixels in `ov_aa*`), but it changes the pixel rule. Measured with `scripts/probes/ov_*.milk`
(regenerated by `scripts/probes/gen_overlay_probes.py`; stroke pixels identical to the oracle, median over 11 frames, 640x360):

| probe | thin quads | wgpu `LineList`/`PointList` (ANV) | `Emulated` |
|---|---|---|---|
| `ov_line` (custom wave strip, 29 k px) | 100.00 % | 100.00 % | 100.00 % |
| `ov_line_short` | 99.94 | 99.94 | 100.00 |
| `ov_line_thick` (4 passes) | 78.62 | 78.62 | 100.00 |
| `ov_serp` (rows at exact sub-pixel phases, 63 k px) | 90.02 | 90.02 | 100.00 |
| `ov_aa` / `_alpha` / `_thick` (shape outlines, GL_LINE_LOOP + smooth) | 99.91 / 99.34 / 99.97 | same | 100.00 |
| `ov_dots` / `ov_dots_thick` | 45.59 / 20.79 | same | 100.00 |
| `ov_mv_max` (3072 isolated GL_LINES) | 50.30 | 50.30 | 100.00 |

* native Vulkan lines on Intel ANV follow the *rectangular* (thin quad) rule: identical to the quads in every probe, so (a)
  is not exact against iris; (b) a fragment-shader emulation would need 64-bit integer arithmetic for the exact ties
  (WGSL has none), so the exact rule is evaluated on the **CPU** (`stroke.rs`, i64 arithmetic after snapping the
  vertices to the 1/256 pixel grid) and every lit pixel is drawn as a 1-px `PointList` vertex at its pixel centre,
  which every GPU rasterises identically. `StrokeRaster::Emulated` is the default, `Native` and `Quads` stay
  selectable (`RenderConfig::stroke`, `Renderer::set_stroke_raster`, `fdrender --stroke`).
* Rules (window space, row 0 = top, 1/256 px snapped):
  * points: pixel `(floor x, floor y)`; custom-wave dots are **all 2n-1 vertices of `SmoothWave`**, always 1 px
    (`gl_PointSize` is ignored, thick dots do not grow);
  * strip (smooth off: waves): x-major (`|dx| >= |dy|`) columns with centre in `[x_lo, x_hi)`, row `floor(y)`; y-major
    rows with centre in `(y_lo, y_hi]`, column `floor(x)`. Exact ties on the minor axis (`y(c)` / `x(c)` an integer
    in the snapped grid, e.g. a wave on a screen edge or through the centre) are **not** `floor`: an x-major segment that
    rises to the right (`dx`, `dy` of opposite sign, image rows down) gives the tie to the upper row (`floor(y) - 1`), a
    falling or flat one to `floor(y)`; a y-major segment always to the left column (`floor(x) - 1`). Measured with
    `scripts/probes/gen_tie_probes.py` (860 tie probes, 100 % identical; independent of drawing direction, slope
    magnitude and the tie's place on the segment). Random off-grid polylines (150 x 4 waves, 80 k pixels) match with
    2 pixels left, rounding (not truncating/ceiling) the 1/256 snap: floor/ceil snapping gives 299 / 359 wrong pixels;
  * smooth lines (outlines, motion vectors): diamond exit: lit iff the segment meets the pixel diamond `D` and its end is
    not in `D`; `D` is open on the lower edges, closed on the upper edges and the right tip.
  * segments are clipped to the GPU's guard band (`+-16384` px around the viewport centre, see "Motion vectors") before
    the vertices are snapped; points beyond it are culled (`stroke::window_segment`, `GUARD_PX`).
  * the warp mesh's triangles are submitted in projectM's order (`c d a`, `d a b`): the GPU's attribute setup depends on the
    order of a triangle's vertices when the per-vertex UVs are huge (`scripts/probes/gen_warp_probes.py`, `wl_pp_dx_1e5`
    39.8 dB -> bit-identical; Redi Jedi - welcome to the underworld(remix) / porthole (hit by a bus edition) `warped` ->
    99 dB; per-pixel `pow(rad, 30)` makes UVs of 1e3-1e23, 1 ulp is several texels).
* custom-wave thick passes offset by `(1/W, 1/W)` ndc (projectM uses `viewportSizeX` for both; half a pixel in x,
  `H/2W` pixels in y); shape thick outlines by `(1/W, 1/H)`; main wave by `(2/W, 2/H)`.
* Shape `y` orientation (oracle README item 4) is **settled**: MD2 also draws `ndc_y = 2y-1` (`Ortho2D` has height `-2`),
  so `y = 0.15` is in the lower part in MD2 and projectM; `ov_shape_orient` is 100 % identical.
* projectM runs **4** custom wave and **4** custom shape slots (`CustomWaveformCount` / `CustomShapeCount`, `Constants.hpp`);
  `wavecode_4..15` / `shapecode_4..15` are never read (MD2: 16). `CompatRules::custom_slots`, applied in `engine.rs` when the
  slots are built. Found with the isolation harness (each enabled shape alone, oracle vs ours): the only preset of the
  4194-file corpus with a slot >= 4 (`bdrv + al EoS - Sarc Gbdrv2 mx_Phat_RedEye_FractalView_v2`, shape 4) went from main 39.3 dB to 91.5 dB.
* Shape outline is drawn only for `border_a > 0.0001` (projectM `CustomShape::Draw`), not `> 0`.
* **Warp shader second output.** projectM's user warp fragment shader writes `texCoords = frag_TEXCOORD0.xy` to a second
  `RG16F` attachment (`layout(location = 1)`), so the interpolated `uv` has two uses in Mesa's NIR and the compiler cannot
  fold constants into its plane equation (`uv - 0.5` stays `plane(uv) - 0.5`). With a single use ANV folds it into
  `plane(uv - 0.5)`, which rounds differently: probe `sin((uv.x-0.5)*100)` showed 15 480 wrong pixels in 13 frames, 0
  after `renderer::with_uv_output` appends the same second output (scratch `warp_sink`, never read). For the same reason
  only `uv_orig`, `rad` and `ang` start the GL varying fold of `docs/hlsl.md` in a warp shader. Remaining warp-shader
  noise is on `rad`/`ang` (probe `sin(ang*20)`: 0.1-2 % of pixels), still open.

## Motion vectors (`motion.rs`)

Measured with `ov_mv*` probes against the oracle's `warpsrc` stage (`fdrender --stages warpsrc` writes ours):

* projectM draws the lines **vertically mirrored**: start `(posX, 1 - posY)`, direction `(dx, -dy)` (flip in the vertex
  shader + `orthogonalProjection` flip, while the UV lookup is not flipped);
* `oldUV` is a texture lookup in the previous frame's `RG16F` warp-UV map (`GL_LINEAR`, 8-bit weights, rounded; floor,
  ceil or 7/9/10 bits are far worse), emulated exactly (diagonal flip of the mesh included): every texel is the
  attribute plane `t0 + a (x - x0) + b (y - y0)` of the mesh triangle, evaluated in f32 from the triangle's vertices
  **snapped to the 1/256 px grid** in the GPU's draw order (upper triangle `c d a`, lower `d a b`), **truncated** to
  half. Against the oracle's own UV map (patched dump, Aderrasi, 320x180) 0-2 of 115 200 texels differ; with the
  unsnapped mesh fractions the lines were a pixel off (mv-s55 `warpsrc` 55-66 dB -> 99 dB). The vertex UVs are the
  GPU's own (`Renderer::gpu_warp_vertex_uv` runs `warp_vs` on the previous frame's inputs: the readback is submitted as its
  own small submission at the end of the frame that produced the inputs, with persistent buffers, and the next frame
  waits for that submission only; `FrameInput::warp_uv` is not needed then, the CPU UVs the lookup falls back on if the
  readback fails are derived from `warp_gpu`). The GPU warp mesh is drawn indexed: one vertex per mesh vertex plus an
  index buffer in the triangle order above;
* the vertex buffer holds `(countX+1)*2` entries that are never cleared and drawn completely after every row; unfilled
  slots (`posX = 1` is skipped, so always the last pair) stay `(0,0)`: an extra line from the bottom-left corner per
  row. The index buffer keeps the vertex count of the first draw, while the GL vertex buffer is re-allocated at every
  frame's count: when `mv_x` shrinks (animated), the indices past the end of the vertex buffer fetch zeros, i.e. more
  corner lines (`suksma - yin - 100` 43 -> 77 dB, `yin - 100` 48 -> 93 dB `warpsrc`);
* min length `1.25 px`, zero length -> diagonal `(min, min)`, frame 0 skipped (as before);
* very long lines (a previous UV far outside the map times `mv_l`): the GPU clips them to its guard band
  (`-16384 ..= 16384` px) instead of dropping them (`stroke::window_segment`); Jester's Surreal Tornado `warpsrc`
  median 58 -> 99 dB.

* **The sampler** (`oracle/probes/sampler_probe.c`, `RG16F` + `GL_LINEAR` + clamp, lookup in a vertex shader, 57 600 random
  lookups per axis, **0 mismatches**): the normalised coordinate is first **truncated to 22 fractional bits**, scaled to
  texels (`- 0.5`), the fraction becomes the weight `k/256` with `k` rounded half up, and the four texels are blended
  exactly (f64 weighted sum rounded to f32; every f32 evaluation order is off in 0.1-3 % of the lookups). The earlier
  f32 `x * w - 0.5` with a rounded weight picked the neighbouring weight for 0.7 % of the lookups: invisible at
  `mv_l = 1`, but one weight step is 0.2 px of line end at `mv_l = 40`. This was the "`mv_l >= 30` lines differ by a
  pixel" open item (Intense Desire `warpsrc` 35-50 dB -> bit-identical on all 30 frames);
* the `RG16F` store is *truncating* **and saturating**: a finite value beyond the half range becomes `+-65504`, never
  infinity (`f16_round`; `dround` frames 16+ `warpsrc` 32 dB -> 99 dB, Picture Of Exile 34 -> 60+ dB). With
  inf / NaN vertex UVs the plane evaluation of the GPU still differs (`-inf` where we give NaN, `-65504` where we give
  `inf`; `dround` frames 14-15 `warpsrc` 41-52 dB), open;
* **a guard-band-clipped smooth line is rasterised reversed** (`oracle/probes/line_probe.c`): the start pixel of a line whose
  far end is clipped is *not* lit (it became the end), the pixel of an inside end *is* lit; GL_LINE_SMOOTH off is not
  affected. The guard band is `+-16384` px **around the viewport centre** (`x` in `W/2 +- 16384`: -16224 ..= 16544 at
  320x180; same at 640x360, 200x300, 1000x200), not around the window origin. Every motion vector of a preset with a huge
  `mv_l` (Picture Of Exile: lines of 1e6-1e7 px) lost its start pixel in the oracle (48 mismatching start pixels in 5
  frames -> 0);
* the rasteriser matches the GPU on random segments: 40 000 random 320x180 lines (5-400 px) differed in 11-12 cases with the
  old round-to-nearest vertex snap, one pixel each, always with an endpoint within 0.008 of a `1/256` tie. Root cause
  (`oracle/probes/snap_probe.c`, ~700 k f32 clip values straddling every pixel boundary, points and smooth / non-smooth
  lines, six viewport sizes, 0 mismatches): the clip unit computes `fma(c, size/2, size/2)` and **truncates it toward zero**
  to f32 before the 1/256 fixed-point conversion (round-to-nearest of that or of `c * h + h` / `(c*0.5+0.5)*size` is off in
  0.5-4 % of the boundary cases). The GPU works with `y` up; `stroke::snap_pos` mirrors the integer result (`256*h - s`).
  Points: pixel = `floor(s_x/256)` and, in image rows, `floor(s_y/256)`; lines keep the tie rules above. Singularity In My
  Oscilloscope frame 7 (two `warpsrc` pixels at the left edge) was such a tie.

## Alpha channel and sampler state (`Compat::ProjectM`, `CompatRules::main_alpha`, `shape_main_nearest`)

Measured against the oracle's `main`/`warped` alpha (96.6-100 % identical pixels on the six
presets with alpha-dependent shapes in free run):

* internal images are RGBA8, cleared to `(0,0,0,0)` (MD2: X8R8G8B8, alpha 1);
* no-shader warp: `colour = (decay,decay,decay,1) * tex`, so alpha = `tex.a` (decay does not scale it); warp *shaders*
  return `(ret, 1)` and write alpha 1;
* overlays (shapes, waves, borders, darken centre, motion vectors) blend with `glBlendFunc(SRC_ALPHA, ONE[_MINUS_SRC_ALPHA])`
  applied to the alpha channel too: `a' = a*a + a_dst*(1-a)` (or `a*a + a_dst` additive);
* textured fills: `diffuse * texel` including alpha, i.e. alpha is multiplied by the sampled `tex.a` (`fs_main_ta`);
* a NaN UV (`pow(zoom<0, ..)` in projectM's GPU vertex shader; CAT Scan's per-pixel `zoom=-1.02+rad*10`) makes the
  GPU sample UV 0 for the whole triangle (flat patch = mean of the image corners under `GL_REPEAT`); such motion-vector
  lines are dropped (`geom::fix_nan_uv_triangles`, `motion::uv_texel`);
* textured custom shapes that sample the main image: the flip texture is `GL_NEAREST`/`GL_REPEAT` (no sampler bound),
  except the first shape draw after a blur update, which still has `BlurTexture`'s linear/clamp sampler bound on
  unit 0 (`Sampler::Unbind(0)` follows it): `Kind::TexLeak`. **Only presets whose shaders sample blur** run the update
  (`blur_levels_required() > 0`; a diagnostic blur chain from `--stages blur*` / `set_min_blur_levels` does not).
  Verified against the oracle with `scripts/probes/ov_texfirst{,_blur}.milk` (from `gen_overlay_probes.py`): step
  ≥ 99 dB on both twins; with the leak disabled the blur twin falls to 52 dB (`main`), the plain twin never leaks.
* first frame after load/resize of a preset whose shaders sample blur (`CompatRules::blur_alloc_drops_overlays`): the
  update that allocates the blur textures restores FBO 0, so shapes, waves, main wave, darken-centre and borders of
  that frame are dropped (`renderer.rs`, `frames_since_resize == 0`). Presets without blur draw them on frame 0.
  Probes: `ov_shapes_blur` / `ov_texfirst_blur` (frame-0 `main` black in the oracle) vs `ov_shapes_plain` / `ov_texfirst`.
* warp shader `sampler_main` (`CompatRules::warp_main_sampler_from_wrap`): `PerPixelMesh::WarpedBlit` binds its own
  `GL_LINEAR` sampler on unit 0 (`GL_REPEAT` if `wrap > 0.0001`, else `GL_CLAMP_TO_EDGE`) after the warp shader loaded
  its samplers, which replaces the sampler of the first `main` descriptor (`std::set` order of the sampler names, `main`
  is always in the set). So a shader's unprefixed `sampler_main` follows `bTexWrap`, not the `GL_REPEAT` that
  `TextureManager::ExtractTextureSettings` gives unqualified names (`run_shader_pass`, `warp_wrap`). Measured: jaben
  "had to cum that" warped 17.8 -> 95.7 dB, tech support for the dead 14.6 -> 99, Furious Spirals 31.6 -> 99, Potion of
  Ink 29.6 -> 97.5 (all `bTexWrap=0`, samples outside `[0,1]` at the borders). The composite shader keeps its named
  samplers.

## Borders (`Border.cpp`, `CompatRules::pm_border_mesh`, `gl_fill_ties`)

* projectM draws each border (outer, then inner; skipped when `a <= 0.001`) as one 8-vertex mesh with the 8 triangles
  `0 1 4 | 1 4 5 | 2 3 6 | 3 7 6 | 2 0 6 | 0 4 6 | 3 7 5 | 1 3 5` (`geom::border_triangles`), mirrored vertically by its
  y-flipping projection. Same pixels as MD2's four rotated quads for sane sizes; negative sizes and `ob + ib > 1`
  (negative inner radius) give self-overlapping triangles, each blended separately, which only the mesh reproduces.
* the oracle rasterises in GL window space (y up): a pixel centre exactly on a *horizontal* triangle edge goes to the
  polygon above it, wgpu / D3D9 give it to the polygon below. Border edges sit on pixel centres whenever
  `size * H / 2` is a half integer (`ob_size = 0.05` at 180 rows, `0.95` ...). `geom::gl_fill_ties` moves every vertex
  of such an edge down by one sub-pixel step (`1/256` px) so wgpu picks the oracle's side; vertical edges need no change.
* the same rule applies to flat custom-shape fills (`renderer::build_overlay`): a 4-, 12-, 20-...-gon at `ang = 0` has
  horizontal top and bottom edges (vertices sit at `ang + pi/4 + 2 pi k / n`), a multiple-of-8-gon has horizontal
  spokes. Only a *boundary* edge (used by one triangle) is a tie: a spoke is shared by two triangles of the fan, its
  pixels are drawn once whichever side owns them. A moved vertex keeps its triangle's colour plane (the colour moves
  along the plane's `d/dy`), otherwise the 1/256-px shift flips colour roundings by one level at a few pixels. Measured
  on step `main` (pixels differing from the oracle over 119 frames): `martin - baby one more time` (flat 12-gon x 256,
  alpha gradient) 506 -> 55 (365 -> 2 pixels off by more than 2 levels); a plain move of every tie vertex gave 141 and
  added one-level errors in `shifter + Geiss - molten glass wrap` (103 -> 110) and `skipper skipper kiss me hard`
  (21 -> 22); spokes moved as well added 21 pixels to `Zylot & Idiot - Face BDRV`. Textured fills are not moved: the
  moved vertex's UV changes the nearest texel at pixel centres (`Zylot - Star Ornament` main 79.0 -> 72.2 dB).
* probes: `scripts/probes/bd_*.milk` (`bd_list.txt`: sizes, ties, negative / huge / overlapping, colours, alpha
  threshold, dynamic sizes, darken-centre), bit-identical (`inf` dB) in `main` and `final` at 320x180, 321x181,
  640x360, 1000x200; at 360x640 `main` is bit-identical and `final` needs the `VideoEcho` aspect quad
  .

## MilkDrop 2 preset blend (`CompatRules::md2_preset_blend`, `blend.rs`)

`FrameInput::md2_blend` (set by the engine while `m_bBlending`) switches `render_frame_inner` to MD2's two-state frame; the engine-side
description is in `docs/engine.md`. Renderer side:

* **Old shaders.** `Renderer::take_shader_state` / `restore_shader_state` / `set_old_shader_state` move a preset's warp / composite
  pass and user textures in and out (`m_shaders` / `m_OldShaders`); `blur_levels_required` counts the old ones. With the rule on, each
  user shader pass also gets two alpha-blended pipelines (`SRCALPHA/INVSRCALPHA`, `INVSRCALPHA/SRCALPHA`); the shader's `ret` alpha is
  the vertex alpha (`_return_value = float4(ret.xyz, _vDiffuse.w)`).
* **Warp** (`run_md2_warp`, `blend::warp_passes`): the blended UV mesh is drawn as one or two passes; triangles are culled by their
  three vertex alpha bytes (`blend::tile_needed`: `(d1|d2|d3) > 0`, or `(d1&d2&d3) < 255` with flipped culling), pass 0 clears and
  draws opaque, pass 1 keeps the target and blends. The fixed-function pass modulates the texel by `(decay, decay, decay, alpha)`.
* **Composite** (`run_md2_comp`, `blend::comp_passes`): `build_noshader_comp` (from the blended post parameters) and / or the
  32x24 `geom::comp_grid` with per-vertex alpha `blend::comp_vertex_alpha` (bilinear lookup of the warp alphas, clamped to cells
  `0..grid-1`, `(int)(alpha*255)`) drawn through `geom::comp_indices` with the same culling. Comp shaders read the previous image
  (`comp_reads_previous`), the fixed-function pass the current one.
* **Overlays.** Shapes / custom waves: `alpha_mult` multiplies the colour alpha before MD2's `(int)(a*255*alpha_mult) & 0xFF`
  (`geom::conv_color_alpha_mult`); the main wave is generated twice when the mode changed (`wavegen::main_wave_geometry_raw`, loop
  closing duplicate only when not blending), the polylines resampled and mixed by `CosineInterp(progress)` (`blend::blend_waves`),
  the alpha mixed likewise and tested against 0.004 afterwards.

## Textured shapes with `image=` (`ShapeInstance::image`, `Kind::TexImage`)

projectM-only key `shapecode_N_image` (`CustomShape.cpp:59,154-175`): a *textured* shape samples a user texture instead of
the main image. Opt-in per shape (`image: None` = everything above). Measured against the oracle with `scripts/probes/si_*`
(`gen_shapeimg_probes.py`, 320x180, 10 frames): see `docs/fidelity.md`.

* texture: looked up by the engine through the `TextureProvider` (name without `fw_/fc_/pw_/pc_` prefix, lower case) and
  registered with `Renderer::set_user_texture`; an absent texture is the 1x1 black dummy (projectM: its 1x1 placeholder);
* sampler: the prefix picks filter (`f` bilinear / `p` nearest) and address (`w` repeat / `c` clamp), default bilinear +
  repeat, level 0 only (projectM's samplers set `MIN/MAG_FILTER` without mipmaps);
* UVs: `u = 0.5 + 0.5*cos(a)/tex_zoom * textureAspectY` with `textureAspectY = 1` for an image (the viewport aspect for the
  main image), `geom::shape_geometry_tex`; the outline keeps the viewport aspect;
* such a draw never takes the "leaked blur sampler" path (the descriptor binds its own sampler);
* the four 2D noise textures projectM registers at start-up (`noise_lq`, `noise_lq_lite`, `noise_mq`, `noise_hq`, exact
  lower-case names) are found before any directory is scanned: `noise::builtin_image_noise` binds the renderer's own noise
  texture (the engine skips them in `resolve_shape_images`). `idlem` / `idleheadphones` (embedded PNGs) are not available;
* the vertical texture coordinate is evaluated as projectM does, `1 - (0.5 - 0.5*sin(a)/tex_zoom)` (same real number as
  `0.5 + ...`, a different `f32`): it flips nearest-filtered texels at ties.

## MilkDrop 3 shader inputs (`ShaderExt`, `Compat::MilkDrop3`)

Behaviour from `docs/md3.md` (shader inputs); the engine side (who computes the values) is described there. The renderer only
transports them, never gates them on `Compat`: outside MilkDrop 3 the engine leaves `ShaderExt::default()` in place, which is
neutral, so output stays bit-identical.

* **Layout.** `WGSL_COMMON` declares, next to the frozen `mu` (`@group(0) @binding(0)`, `MilkUniforms`), the extension block
  `@group(0) @binding(1) var<uniform> mx: MilkExt` (`lactea_abi::MilkExtUniforms`, `BINDING_MILK_EXT`, fragment stage):
  `mx.mouse` = `_c14` and `mx.q_hi[0..8]` = `_qi.._qp` (`q33 == mx.q_hi[0].x`, `q64 == mx.q_hi[7].w`). `Pipelines::bgl_mu` always
  has both entries and `run_shader_pass_ex` always uploads both buffers, so a shader that does not declare `mx` is unaffected
  (the layout is a superset of what it uses).
* **Who fills what.** Every warp / composite shader pass takes a `ShaderExt`: `FrameInput::ext` for the state the frame is built
  for, `Md2OldPass::ext` for the *old* shader passes of a MilkDrop 2 / 3 preset blend (`run_md2_warp` / `run_md2_comp`, `Which::Old`):
  the old state's `_c14`, `q33..q64` and shader time are its own, like its `q1..q32` and blur range.
  * `mouse`: `(px/W, py/H, left held 0/1, released 1-frame 0/1)`, y from the top (matches `uv.y`), `(-1, -1, ..)` while the cursor
    is outside the window; default `(-1, -1, 0, 0)`.
  * `q_hi`: `q33..q64` after the state's per-frame code; default zeros.
* **Shader time.** `fill_milk_uniforms` takes `_c2.x` from `ShaderExt::time` (seconds since *this state's* preset started, `None` =
  `FrameInput::time`, as before) and wraps it modulo 10000 like the absolute time (`shader_time_wrap`, see "Noise and shader time"; start 100 s, now 12445.5 s: the engine passes
  12345.5, the shader sees 2345.5). `_c8.._c11` and the `rot_*` matrices keep using `FrameInput::time`.
* **FFT texture.** Each `Renderer` owns a 512 x 1 `R32Float` texture (`TEXTURE_BINDING | COPY_DST`, zero until the first upload,
  created in `Renderer::new`, so `sibling()` gets its own) that `TexSource::Fft` bindings sample (`texsize` = `(512, 1, 1/512, 1)`).
  A frame with `ext.fft = Some(..)` replaces its contents before any pass is recorded (`render_frame` and
  `render_frame_to_output`); `None` keeps the previous contents; the old state's `fft` is ignored (renderer-global). The upload is a
  staging-buffer copy recorded in the frame's encoder, not `queue.write_texture`: a queue write lands at the next submit ahead of all
  recorded work, so two frames of one renderer recorded into one encoder would see the last spectrum.
  `R32Float` is not filterable without `FLOAT32_FILTERABLE`: the group(1) layout entry of a `TexSource::Fft` binding is
  `Float { filterable: false }` (its sampler entry stays `Filtering` and is never paired with it) and the translated WGSL does
  `textureLoad` with its own clamp + bilinear. The dimensionality check treats `Fft` as a 2D texture.
* tests: `tests/shader_ext.rs` (GPU: mouse, `q_hi`, FFT upload / keep / per-renderer / encoder order, old-state ext during a blend),
  `uniforms.rs` (`_c2.x` wrap, neutral default).

## BeatDrop shader inputs (`CompatRules::md2_shader_inputs`, `Compat::MilkDrop2` only)

Off under projectM (`sampler_fft` stays an unresolved user texture) and MilkDrop 3 (own prelude); the engine computes the values (`docs/hlsl.md`
"BeatDrop prelude", `docs/audio.md` `BeatDropShaderFft`), the renderer transports them.

* **Textures.** With the rule on, the renderer's FFT texture is `BEATDROP_FFT_BINS` (1024) x 2 `R32Float` instead of 512 x 1 (row 0 smoothed spectrum, row 1 peak hold;
  BeatDrop `plugin.cpp:2404-2408`, `MY_FFT_SHADER_SAMPLES` = 1024 in `plugin.h:70`), and a second, separate `BEATDROP_WAVE_SAMPLES` (480) x 2 `R32Float` texture holds the
  waveform (row 0 left, row 1 right; `NUM_WAVEFORM_SAMPLES`). `BEATDROP_ROWS` no longer exists. The wave texture has no `TexSource` in the frozen ABI: the translator
  names it the user texture `"wave"` and `Renderer::engine_wave` answers `User("wave")` with it before host textures are consulted (the engine never asks the host's
  texture provider for `"wave"` under MilkDrop 2). `ShaderExt::beatdrop: Option<Arc<BeatDropTexture>>` replaces both textures before any pass is recorded
  (`record_beatdrop_upload`, staging copies in the frame's encoder like the MD3 upload); packed upload order: spectrum row, peak row, wave L, wave R
  (`BeatDropTexture::texels`); `None` keeps them; `ShaderExt::fft` is ignored under the rule. Renderer-global: the old state's `beatdrop` is ignored.
  `texsize_fft` = `(1024, 2, ..)` and `texsize_wave` = `(480, 2, ..)`, the real sizes.
* **Constants.** `_c14` is `ShaderExt::mouse`; `_c15` (`hour, minute, second, totalSeconds`) and `_c16` (`year, month, day, weekday`) travel in
  `ShaderExt::time_date`, written into `MilkExtUniforms::q_hi[0..2]` (`None` leaves `q_hi` as it is). The `lactea-abi` layout is unchanged. The old state of
  a blend carries the same frame values.
* tests: `crates/lactea/tests/md2_inputs.rs`, `crates/lactea-hlsl/tests/beatdrop.rs`.
* `CompatRules::md2_input_vars` (MD2 true; projectM / MD3 false) is the expression-side twin: it only makes the engine register `mousex .. totalseconds` in the preset's
  contexts (`docs/eel.md`); the renderer does not read it. `md2_shader_inputs` also selects the 1024 x 2 FFT texture size at `Renderer::new` and creates the 480 x 2 wave texture.
