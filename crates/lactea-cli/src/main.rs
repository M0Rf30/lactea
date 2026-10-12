//! `fdrender`: deterministic headless renderer producing oracle-layout dumps.
//!
//! Mirrors `oracle/refrender` (same flags, same output layout, same driving rules) and adds
//! teacher forcing:
//!
//! ```text
//! fdrender --preset P.milk --audio fixtures/audio/synth128.wav --out out/fd/x --frames 120 --vars
//! fdrender ... --inject-noise out/ref/starter/x      # free run, oracle's noise textures
//! fdrender ... --teacher     out/ref/starter/x      # single-step mode (see below)
//! ```
//!
//! `--teacher REF` renders every frame `n` from the oracle's state: before frame `n > 0` the
//! reference `main[n-1]` becomes the image the warp samples, the reference per-frame variables
//! of frame `n` (incl. `q*`, `reg*`, user variables, `audio.*`), the per-shader random state
//! (`warp.* / comp.*` `rand_*` and `rot_*`) and the hue offsets overwrite ours, and the
//! reference noise textures are loaded. Errors therefore cannot accumulate: each stage diff of
//! frame `n` isolates one pass.

mod gpu;
mod textures;
mod wav;

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use lactea::{
    Compat, Engine, EngineConfig, ForcedFrame, FrameClock, MouseState, PresetReport, Transition,
    TransitionKind,
};
use lactea_abi::dump::{DumpMeta, Stage};
use lactea_abi::{NoiseTex, RunConfig, pcm_range};
use lactea_compare::{DumpReader, DumpWriter};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

const PRODUCER: &str = concat!(
    "lactea@",
    env!("CARGO_PKG_VERSION"),
    "+",
    env!("FD_GIT_SHA")
);

#[derive(Parser, Debug)]
#[command(
    name = "fdrender",
    version,
    about = "lactea deterministic headless renderer"
)]
struct Args {
    #[arg(long)]
    preset: PathBuf,
    #[arg(long)]
    audio: PathBuf,
    #[arg(long)]
    out: PathBuf,
    /// WxH
    #[arg(long, default_value = "640x360")]
    size: String,
    /// WxH warp mesh cells
    #[arg(long, default_value = "48x36")]
    mesh: String,
    #[arg(long, default_value_t = 60)]
    fps: u32,
    #[arg(long, default_value_t = 300)]
    frames: u32,
    #[arg(long, default_value_t = 1)]
    seed: u64,
    /// X,Y in texels
    #[arg(long, default_value = "0,0")]
    texel_offset: String,
    /// final,main,warped,blur1,blur2,blur3 | all | none
    #[arg(long, default_value = "final,main,warped,blur1,blur2,blur3")]
    stages: String,
    /// write vars/NNNNN.json
    #[arg(long)]
    vars: bool,
    /// write the noise textures to textures/
    #[arg(long)]
    textures: bool,
    /// user texture directory (default: oracle/build/texpack, the pack the oracle used)
    #[arg(long)]
    texture_dir: Option<PathBuf>,
    /// teacher forcing from an oracle dump directory (implies --inject-noise)
    #[arg(long)]
    teacher: Option<PathBuf>,
    /// with --teacher: also feed the oracle's `main[n]` to the composite shader (isolates the
    /// composite pass from `main` errors; the `main` dump stays ours)
    #[arg(long)]
    comp_from_ref: bool,
    /// use the noise textures of an oracle dump directory (free run)
    #[arg(long)]
    inject_noise: Option<PathBuf>,
    /// projectm (default) | md2 | md3 (aliases milkdrop2, milkdrop3) | auto. `auto` picks projectm or md3 per
    /// preset from its content (md2 only as the fallback for a preset projectm rejects with a compile error; a
    /// mode change on `--preset2` is a hard cut) and reports the choice in fdrender.json (`compat_auto`,
    /// `compat_evidence`); the default stays projectm.
    #[arg(long, default_value = "projectm", value_parser = parse_compat)]
    compat: CompatChoice,
    /// line/point rasteriser: native | emulated | quads (default: the renderer's default)
    #[arg(long, value_parser = parse_stroke)]
    stroke: Option<lactea_render::StrokeRaster>,
    /// MilkDrop 3 (`--compat md3`, or `auto` when the preset resolves to md3): a fixed mouse for every frame,
    /// `X,Y[,down]` with X, Y the cursor as fractions of the image (0..1, origin top left); `down` presses the
    /// left button. Default: the cursor is outside the window.
    #[arg(long, value_name = "X,Y[,down]", value_parser = parse_mouse)]
    mouse: Option<MouseState>,
    /// MilkDrop 3 `VolumeSensitivity` (`--compat md3`, or `auto` with an md3 preset): linear gain on the 8-bit quantised audio input.
    #[arg(long, default_value_t = 1.0)]
    volume_sensitivity: f32,
    /// MilkDrop 3 encrypted warp shader (`MD31` handle): replace the preset's placeholder warp shader with the HLSL in
    /// FILE. Repeatable. The HLSL must be one you own; see docs/md3.md, "Encrypted shaders".
    #[arg(long, value_name = "HANDLE=FILE")]
    shader_override_warp: Vec<String>,
    /// MilkDrop 3 encrypted composite shader (`MD32` handle): replace the preset's placeholder composite shader with
    /// the HLSL in FILE. Repeatable.
    #[arg(long, value_name = "HANDLE=FILE")]
    shader_override_comp: Vec<String>,
    /// second preset: replaced at --switch-frame by a soft cut of --soft-cut seconds
    #[arg(long)]
    preset2: Option<PathBuf>,
    /// frame at which --preset2 replaces the first preset (default 0; needs --preset2)
    #[arg(long)]
    switch_frame: Option<u32>,
    /// soft cut duration in seconds (default 3 like projectM; needs --preset2)
    #[arg(long)]
    soft_cut: Option<f64>,
    /// 1 = the second preset starts from black, 0 = from the first preset's image (projectM default; needs --preset2)
    #[arg(long)]
    start_clean: Option<u8>,
    /// force the transition shader (0 circle, 1 plasma, 2 simple blend, 3 sweep, 4 warp, 5 zoom blur)
    #[arg(long)]
    transition_shader: Option<u32>,
    /// force the four static random integers `a,b,c,d` of the transition
    #[arg(long)]
    transition_static: Option<String>,
    /// with a reference that has a transition: feed its `old_out` / `new_out` images to the blend
    /// pass (isolates the transition shader; per-preset stages stay ours)
    #[arg(long)]
    blend_teacher: Option<PathBuf>,
    /// MilkDrop 3 AutoHardCutMode 0..=7 (0 = off; acts only with --compat md3, or auto with an md3 preset). fdrender has no preset
    /// switching, so the instant preset changes of modes 1..=6 are only counted ("auto_cuts" in
    /// fdrender.json); mode 7 injects its effects into the composite shader, so the frames show them
    #[arg(long, default_value_t = 0)]
    auto_hard_cut: u8,
}

/// `--mouse X,Y[,down]`.
fn parse_mouse(s: &str) -> Result<MouseState, String> {
    let mut it = s.split(',');
    let mut coord = |what: &str| -> Result<f32, String> {
        it.next()
            .ok_or_else(|| format!("expected X,Y[,down], got {s:?}"))?
            .trim()
            .parse::<f32>()
            .map_err(|e| format!("{what}: {e}"))
    };
    let (x, y) = (coord("X")?, coord("Y")?);
    let left_down = match it.next().map(str::trim) {
        None => false,
        Some("down") => true,
        Some(o) => return Err(format!("the third field is `down`, got {o:?}")),
    };
    Ok(MouseState {
        position: Some([x, y]),
        left_down,
    })
}

/// `--compat` value: a fixed mode, or `auto` (resolved per preset by the engine).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompatChoice {
    Auto,
    Fixed(Compat),
}

/// `--compat`: `auto`, `projectm`, `md2`/`milkdrop2`, `md3`/`milkdrop3`.
fn parse_compat(s: &str) -> Result<CompatChoice, String> {
    match s {
        "auto" => Ok(CompatChoice::Auto),
        "projectm" => Ok(CompatChoice::Fixed(Compat::ProjectM)),
        "md2" | "milkdrop2" => Ok(CompatChoice::Fixed(Compat::MilkDrop2)),
        "md3" | "milkdrop3" => Ok(CompatChoice::Fixed(Compat::MilkDrop3)),
        o => Err(format!(
            "expected projectm|md2|md3|auto (or milkdrop2|milkdrop3), got {o:?}"
        )),
    }
}

/// `--stroke`: `native`, `emulated` or `quads`.
fn parse_stroke(s: &str) -> Result<lactea_render::StrokeRaster, String> {
    use lactea_render::StrokeRaster as S;
    match s {
        "native" => Ok(S::Native),
        "emulated" => Ok(S::Emulated),
        "quads" => Ok(S::Quads),
        o => Err(format!("expected native|emulated|quads, got {o:?}")),
    }
}

fn parse_pair<T: std::str::FromStr>(s: &str, sep: char, what: &str) -> Result<(T, T)> {
    let (a, b) = s
        .split_once(sep)
        .ok_or_else(|| anyhow!("{what}: expected A{sep}B, got {s:?}"))?;
    Ok((
        a.trim()
            .parse()
            .map_err(|_| anyhow!("{what}: bad number {a:?}"))?,
        b.trim()
            .parse()
            .map_err(|_| anyhow!("{what}: bad number {b:?}"))?,
    ))
}

fn parse_stages(s: &str) -> Result<Vec<Stage>> {
    let s = s.trim();
    if s == "none" || s.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for tok in s.split(',') {
        match tok.trim() {
            "all" => out.extend(Stage::ALL),
            // non-ABI extras of the oracle (previous main + motion vectors; soft cut images)
            "warpsrc" | "old_out" | "new_out" => {}
            t => out.push(
                lactea_compare::dump::parse_stage(t)
                    .ok_or_else(|| anyhow!("unknown stage {t:?}"))?,
            ),
        }
    }
    out.sort_by_key(|s| Stage::ALL.iter().position(|x| x == s));
    out.dedup();
    Ok(out)
}

fn noise_names() -> [(NoiseTex, &'static str); 6] {
    [
        (NoiseTex::Lq, "noise_lq"),
        (NoiseTex::LqLite, "noise_lq_lite"),
        (NoiseTex::Mq, "noise_mq"),
        (NoiseTex::Hq, "noise_hq"),
        (NoiseTex::VolLq, "noisevol_lq"),
        (NoiseTex::VolHq, "noisevol_hq"),
    ]
}

/// The oracle's `FDV3` volume file: 28-byte header then RGBA8 `w*h*d*4`.
fn volume_payload(bytes: &[u8]) -> Result<&[u8]> {
    if bytes.len() < 28 || u32::from_le_bytes(bytes[0..4].try_into()?) != 0x3356_4446 {
        bail!("not an FDV3 volume");
    }
    Ok(&bytes[28..])
}

fn inject_oracle_noise(engine: &mut Engine, reference: &DumpReader) -> Result<()> {
    for (tex, name) in noise_names() {
        let data = if matches!(tex, NoiseTex::VolLq | NoiseTex::VolHq) {
            let raw = reference.load_texture_bin(name)?;
            volume_payload(&raw)?.to_vec()
        } else {
            reference.load_texture(name)?.rgba
        };
        engine
            .renderer_mut()
            .inject_noise(tex, &data)
            .map_err(|e| anyhow!("inject {name}: {e}"))?;
    }
    Ok(())
}

/// Our own noise textures in the dump layout (same stream the renderer uses at construction).
fn write_own_noise(writer: &DumpWriter, seed: u64) -> Result<()> {
    use lactea_render::{Rng, noise};
    let mut rng = Rng::new(seed).fork(0x6E6F_6973);
    for (tex, name) in noise_names() {
        let data = noise::generate(tex, &mut rng);
        let (size, _, vol) = noise::noise_spec(tex);
        if vol {
            let mut bin = Vec::with_capacity(28 + data.len());
            for v in [0x3356_4446u32, 1, size, size, size, 4, 0] {
                bin.extend_from_slice(&v.to_le_bytes());
            }
            bin.extend_from_slice(&data);
            writer.write_texture_bin(name, &bin)?;
        } else {
            writer.write_texture(name, size, size, &data)?;
        }
    }
    Ok(())
}

fn default_texture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../oracle/build/texpack")
}

/// A non-ABI image stage: `<out>/<name>/<frame:05>.png`.
fn write_extra(out: &Path, name: &str, n: u32, w: u32, h: u32, rgba: Vec<u8>) -> Result<()> {
    let dir = out.join(name);
    std::fs::create_dir_all(&dir)?;
    lactea_compare::dump::Image {
        width: w,
        height: h,
        rgba,
    }
    .save_png(dir.join(format!("{n:05}.png")))?;
    Ok(())
}
/// `--size WxH`; the `x` is case-insensitive like the app's `--size`.
fn parse_size(s: &str) -> Result<(u32, u32)> {
    parse_pair(&s.replace('X', "x"), 'x', "--size")
}

/// Cross-flag checks that must fail before `run` creates the output directory.
fn validate(args: &Args) -> Result<()> {
    if let Some(d) = &args.texture_dir
        && !d.is_dir()
    {
        bail!("--texture-dir: {} is not a directory", d.display());
    }
    if args.preset2.is_some() {
        let at = args.switch_frame.unwrap_or(0);
        if at >= args.frames {
            bail!(
                "--switch-frame {at} is not below --frames {}: the second preset would never load",
                args.frames
            );
        }
    } else {
        let orphan = [
            ("--switch-frame", args.switch_frame.is_some()),
            ("--soft-cut", args.soft_cut.is_some()),
            ("--start-clean", args.start_clean.is_some()),
            ("--transition-shader", args.transition_shader.is_some()),
            ("--transition-static", args.transition_static.is_some()),
        ]
        .into_iter()
        .find(|(_, given)| *given);
        if let Some((flag, _)) = orphan {
            bail!("{flag} needs --preset2");
        }
    }
    Ok(())
}

fn run(args: &Args) -> Result<i32> {
    let (width, height) = parse_size(&args.size)?;
    let (mx, my): (u32, u32) = parse_pair(&args.mesh, 'x', "--mesh")?;
    if width == 0 || height == 0 {
        anyhow::bail!("--size: width and height must be >= 1 (got {width}x{height})");
    }
    if mx < 2 || my < 2 || mx % 2 != 0 || my % 2 != 0 {
        anyhow::bail!("--mesh: cell counts must be even and >= 2 (got {mx}x{my})");
    }
    let (tox, toy): (f32, f32) = parse_pair(&args.texel_offset, ',', "--texel-offset")?;
    let stages = parse_stages(&args.stages)?;
    // non-ABI extra stage of the oracle: previous main + this frame's motion vectors
    let want_warpsrc = args.stages.split(',').any(|t| t.trim() == "warpsrc");
    let auto_compat = args.compat == CompatChoice::Auto;
    let base_compat = match args.compat {
        CompatChoice::Auto => Compat::ProjectM,
        CompatChoice::Fixed(c) => c,
    };
    if args.auto_hard_cut > lactea::auto_cut::AUTO_HARD_CUT_MODES {
        bail!(
            "--auto-hard-cut: expected 0..={}, got {}",
            lactea::auto_cut::AUTO_HARD_CUT_MODES,
            args.auto_hard_cut
        );
    }
    if width == 0 || height == 0 || args.fps == 0 {
        bail!("size and fps must be positive");
    }

    let preset_bytes = std::fs::read(&args.preset)
        .with_context(|| format!("cannot read preset {}", args.preset.display()))?;
    let audio = wav::load_wav(&args.audio)?;
    let preset2_bytes = match &args.preset2 {
        Some(p) => {
            Some(std::fs::read(p).with_context(|| format!("cannot read preset {}", p.display()))?)
        }
        None => None,
    };
    let forced_shader = match args.transition_shader {
        Some(k) if k < 6 => Some(TransitionKind::from_index(k)),
        Some(k) => bail!("--transition-shader: expected 0..5, got {k}"),
        None => None,
    };
    let forced_static = match &args.transition_static {
        Some(s) => {
            let v: Vec<i32> = s
                .split(',')
                .map(|x| {
                    x.trim()
                        .parse::<i32>()
                        .map_err(|_| anyhow!("--transition-static: bad integer {x:?}"))
                })
                .collect::<Result<_>>()?;
            let a: [i32; 4] = v
                .try_into()
                .map_err(|_| anyhow!("--transition-static: expected a,b,c,d"))?;
            Some(a)
        }
        None => None,
    };
    let blend_ref = args.blend_teacher.clone();
    // `old_out` / `new_out` are extra stages of a soft cut; `all` includes them only when a
    // second preset (`--preset2`) makes a soft cut happen.
    let stage_tok = |t: &str| {
        let extra_soft_cut_stage = matches!(t, "old_out" | "new_out");
        args.stages.split(',').any(|x| {
            let x = x.trim();
            x == t || (x == "all" && args.preset2.is_some() && extra_soft_cut_stage)
        })
    };
    let want_old_out = stage_tok("old_out");
    let want_new_out = stage_tok("new_out");
    let mut report2: Option<PresetReport> = None;
    let mut transition_json: Option<serde_json::Value> = None;

    let reference = match (&args.teacher, &args.inject_noise) {
        (Some(d), _) | (None, Some(d)) => {
            Some(DumpReader::open(d).with_context(|| format!("reference dump {}", d.display()))?)
        }
        _ => None,
    };
    let teacher = args.teacher.is_some();
    if teacher && !reference.as_ref().is_some_and(|r| r.has_vars()) {
        bail!("--teacher needs per-frame vars in the reference dump");
    }

    let run_cfg = RunConfig {
        width,
        height,
        mesh: [mx, my],
        fps: args.fps,
        frames: args.frames,
        seed: args.seed,
        texel_offset: [tox, toy],
    };
    let writer = DumpWriter::create(
        &args.out,
        DumpMeta {
            producer: PRODUCER.to_string(),
            preset: args.preset.display().to_string(),
            audio: args.audio.display().to_string(),
            config: run_cfg,
            stages: stages.clone(),
            has_vars: args.vars,
        },
    )?;

    let mut shader_overrides = std::collections::HashMap::<u64, lactea::ShaderOverride>::new();
    for (spec, is_warp) in args
        .shader_override_warp
        .iter()
        .map(|s| (s, true))
        .chain(args.shader_override_comp.iter().map(|s| (s, false)))
    {
        let flag = if is_warp {
            "--shader-override-warp"
        } else {
            "--shader-override-comp"
        };
        let Some((handle, file)) = spec.split_once('=') else {
            bail!("{flag}: expected HANDLE=FILE, got {spec:?}");
        };
        let handle: u64 = handle
            .trim()
            .parse()
            .map_err(|e| anyhow::anyhow!("{flag}: bad handle {handle:?}: {e}"))?;
        let text = std::fs::read_to_string(file)
            .map_err(|e| anyhow::anyhow!("{flag}: reading {file}: {e}"))?;
        let entry = shader_overrides.entry(handle).or_default();
        if is_warp {
            entry.warp = Some(text);
        } else {
            entry.comp = Some(text);
        }
    }

    let gpu = gpu::init()?;
    eprintln!("fdrender: adapter {}", gpu.adapter_name);
    let mut engine = Engine::new(
        &gpu.device,
        &gpu.queue,
        EngineConfig {
            width,
            height,
            mesh: [mx, my],
            seed: args.seed,
            compat: base_compat,
            auto_compat,
            texel_offset: [tox, toy],
            output_format: wgpu::TextureFormat::Rgba8Unorm,
            sample_rate: audio.rate,
            fps: args.fps,
            preset_duration: 1e9,
            start_clean: args.start_clean.unwrap_or(0) != 0,
            auto_hard_cut_mode: args.auto_hard_cut,
            volume_sensitivity: args.volume_sensitivity,
            shader_overrides,
            ..EngineConfig::default()
        },
    );
    if !auto_compat && args.auto_hard_cut > 0 && !engine.auto_hard_cut_active() {
        eprintln!("fdrender: --auto-hard-cut needs --compat md3, ignored");
    }
    let tex_dir = args.texture_dir.clone().unwrap_or_else(default_texture_dir);
    let provider = textures::DirTextures::scan(&tex_dir);
    eprintln!(
        "fdrender: {} user textures under {}",
        provider.len(),
        tex_dir.display()
    );
    engine.set_texture_provider(Box::new(provider));
    let blur_needed = stages
        .iter()
        .map(|s| match s {
            Stage::Blur1 => 1,
            Stage::Blur2 => 2,
            Stage::Blur3 => 3,
            _ => 0,
        })
        .max()
        .unwrap_or(0);
    engine.renderer_mut().set_min_blur_levels(blur_needed);
    if let Some(s) = args.stroke {
        engine.renderer_mut().set_stroke_raster(s);
    }
    if !auto_compat && args.volume_sensitivity != 1.0 && base_compat != Compat::MilkDrop3 {
        eprintln!("fdrender: --volume-sensitivity needs --compat md3, ignored");
    }
    if let Some(m) = args.mouse {
        if !auto_compat && base_compat != Compat::MilkDrop3 {
            eprintln!("fdrender: --mouse needs --compat md3, ignored");
        }
        engine.set_mouse(m);
    }

    if let Some(r) = &reference {
        inject_oracle_noise(&mut engine, r)?;
    }
    if args.textures {
        if let Some(r) = &reference {
            // copy the textures actually used
            for (_, name) in noise_names() {
                if name.starts_with("noisevol") {
                    writer.write_texture_bin(name, &r.load_texture_bin(name)?)?;
                } else {
                    let img = r.load_texture(name)?;
                    writer.write_texture(name, img.width, img.height, &img.rgba)?;
                }
            }
        } else {
            write_own_noise(&writer, args.seed)?;
        }
    }

    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let load = engine.load_preset(
        &preset_bytes,
        &args.preset.display().to_string(),
        Transition::Cut,
    );
    let load_err = pollster::block_on(scope.pop());
    let report = match load {
        Ok(r) => r,
        Err(e) => {
            eprintln!("fdrender: PRESET LOAD FAILED: {e}");
            write_info(
                &args.out,
                serde_json::json!({
                    "producer": PRODUCER,
                    "preset_loaded": false,
                    "preset_error": e.to_string(),
                    "frames_rendered": 0,
                }),
            )?;
            return Ok(2);
        }
    };
    let mut validation_errors: Vec<String> = Vec::new();
    if let Some(e) = load_err {
        validation_errors.push(format!("load: {e}"));
    }
    for w in &report.warnings {
        eprintln!("fdrender: warning: {w}");
    }
    for f in &report.shader_fallbacks {
        eprintln!(
            "fdrender: SHADER FALLBACK ({:?}): {} -> {}",
            f.pass, f.reason, f.fallback
        );
    }
    let evidence = if auto_compat {
        let (name, ev) = detect_note(&preset_bytes, &report);
        eprintln!("fdrender: compat auto -> {name} (evidence: {ev})");
        if report.compat != Compat::MilkDrop3 {
            if args.mouse.is_some() {
                eprintln!("fdrender: --mouse needs an md3 preset under --compat auto, ignored");
            }
            if args.volume_sensitivity != 1.0 {
                eprintln!(
                    "fdrender: --volume-sensitivity needs an md3 preset under --compat auto, ignored"
                );
            }
            if args.auto_hard_cut > 0 {
                eprintln!(
                    "fdrender: --auto-hard-cut needs an md3 preset under --compat auto, ignored"
                );
            }
        }
        detected_evidence(&preset_bytes, &report)
    } else {
        Vec::new()
    };
    let mut evidence2: Option<Vec<String>> = None;
    for t in &report.unresolved_textures {
        eprintln!("fdrender: unresolved texture {t}");
    }

    let target = gpu::create_target(&gpu.device, width, height);
    let sr = u64::from(audio.rate);
    let zero = vec![0.0f32; 4096 * 2];
    let t0 = Instant::now();
    let mut auto_cuts = 0u64;
    let mut done = 0u32;
    for n in 0..args.frames {
        // audio: PCM frames [s0, e0), padded with silence once the file is exhausted
        let (s0, e0) = pcm_range(u64::from(n), sr, u64::from(args.fps));
        if s0 < audio.frames {
            let end = e0.min(audio.frames);
            engine.push_pcm_f32(&audio.data[(s0 * 2) as usize..(end * 2) as usize], 2);
        }
        let mut missing = if s0 >= audio.frames {
            e0 - s0
        } else {
            e0.saturating_sub(audio.frames)
        };
        while missing > 0 {
            let c = missing.min(4096);
            engine.push_pcm_f32(&zero[..(c * 2) as usize], 2);
            missing -= c;
        }

        // preset switch (refrender: top of the iteration, before PCM / frame time)
        if let (Some(p2), Some(p2_path), true) = (
            &preset2_bytes,
            &args.preset2,
            n == args.switch_frame.unwrap_or(0),
        ) {
            engine.force_next_transition(forced_shader, forced_static);
            let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
            let load2 = engine.load_preset(
                p2,
                &p2_path.display().to_string(),
                Transition::Soft {
                    seconds: args.soft_cut.unwrap_or(3.0),
                },
            );
            if let Some(e) = pollster::block_on(scope.pop()) {
                validation_errors.push(format!("load preset2: {e}"));
            }
            match load2 {
                Ok(r) => {
                    for w in &r.warnings {
                        eprintln!("fdrender: warning (preset2): {w}");
                    }
                    for f in &r.shader_fallbacks {
                        eprintln!(
                            "fdrender: SHADER FALLBACK preset2 ({:?}): {} -> {}",
                            f.pass, f.reason, f.fallback
                        );
                    }
                    if auto_compat {
                        let (name, ev) = detect_note(p2, &r);
                        eprintln!("fdrender: compat auto -> {name} (preset2; {ev})");
                        evidence2 = Some(detected_evidence(p2, &r));
                    }
                    report2 = Some(r);
                }
                Err(e) => {
                    eprintln!("fdrender: PRESET2 LOAD FAILED: {e}");
                    write_info(
                        &args.out,
                        serde_json::json!({
                            "producer": PRODUCER,
                            "preset_loaded": true,
                            "preset2_loaded": false,
                            "preset2_error": e.to_string(),
                            "frames_rendered": done,
                        }),
                    )?;
                    return Ok(2);
                }
            }
        }

        if let Some(r) = reference.as_ref().filter(|_| teacher) {
            let vars = r.load_vars(n)?;
            let prev_main = if n > 0 {
                let img = r.load_stage(Stage::Main, n - 1)?;
                if img.width != width || img.height != height {
                    bail!(
                        "reference main is {}x{}, run is {width}x{height}",
                        img.width,
                        img.height
                    );
                }
                Some(img.rgba)
            } else {
                None
            };
            let comp_main = if args.comp_from_ref {
                let img = r.load_stage(Stage::Main, n)?;
                if img.width != width || img.height != height {
                    bail!(
                        "reference main is {}x{}, run is {width}x{height}",
                        img.width,
                        img.height
                    );
                }
                Some(img.rgba)
            } else {
                None
            };
            engine.force_next_frame(Some(ForcedFrame {
                vars,
                prev_main,
                comp_main,
            }));
            // the incoming preset of a soft cut: its own per-frame vars and previous main (the
            // reference only has them on frames where the transition is drawn)
            let new_vars = r.dir().join("new_vars").join(format!("{n:05}.json"));
            if new_vars.is_file() {
                let vars = lactea_compare::dump::parse_vars(&std::fs::read_to_string(&new_vars)?)
                    .map_err(|e| anyhow!("{}: {e}", new_vars.display()))?;
                let prev = r
                    .dir()
                    .join("new_main")
                    .join(format!("{:05}.png", n.wrapping_sub(1)));
                let prev_main = if n > 0 && prev.is_file() {
                    Some(lactea_compare::dump::Image::load_png(&prev)?.rgba)
                } else {
                    None
                };
                engine.force_next_incoming_frame(Some(ForcedFrame {
                    vars,
                    prev_main,
                    comp_main: None,
                }));
            }
        }
        if let Some(r) = &blend_ref {
            let (o, nw) = (
                r.join("old_out").join(format!("{n:05}.png")),
                r.join("new_out").join(format!("{n:05}.png")),
            );
            if o.is_file() && nw.is_file() {
                engine.force_next_blend_inputs(Some((
                    lactea_compare::dump::Image::load_png(&o)?.rgba,
                    lactea_compare::dump::Image::load_png(&nw)?.rgba,
                )));
            }
        }

        let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        engine.render(
            &mut enc,
            &target.view,
            FrameClock {
                time: f64::from(n) / f64::from(args.fps),
                frame: n,
            },
        );
        auto_cuts += u64::from(engine.take_auto_cuts());
        gpu.queue.submit(Some(enc.finish()));
        if let Some(e) = pollster::block_on(scope.pop())
            && validation_errors.len() < 20
        {
            validation_errors.push(format!("frame {n}: {e}"));
        }

        for &st in &stages {
            let (w, h, data) = if st == Stage::Final {
                (width, height, gpu::read_rgba8(&gpu, &target)?)
            } else {
                engine
                    .renderer()
                    .read_stage(st)
                    .map_err(|e| anyhow!("read {}: {e}", st.dir()))?
            };
            writer.write_stage(st, n, w, h, &data)?;
        }
        if want_warpsrc {
            let (w, h, data) = engine
                .renderer()
                .read_warp_source()
                .map_err(|e| anyhow!("read warpsrc: {e}"))?;
            write_extra(&args.out, "warpsrc", n, w, h, data)?;
        }
        if args.vars {
            let vars: BTreeMap<String, f64> = engine.frame_vars().into_iter().collect();
            writer.write_vars(n, &vars)?;
        }
        // soft cut frames: the two images fed to the transition and the incoming preset's own stages
        if let (Some(_), Some(inc)) = (engine.transition_progress(), engine.incoming_renderer()) {
            if want_old_out {
                let (w, h, d) = engine
                    .renderer()
                    .read_stage(Stage::Final)
                    .map_err(|e| anyhow!("read old_out: {e}"))?;
                write_extra(&args.out, "old_out", n, w, h, d)?;
            }
            if want_new_out {
                let (w, h, d) = inc
                    .read_stage(Stage::Final)
                    .map_err(|e| anyhow!("read new_out: {e}"))?;
                write_extra(&args.out, "new_out", n, w, h, d)?;
            }
            for &st in stages.iter().filter(|s| **s != Stage::Final) {
                let (w, h, d) = inc
                    .read_stage(st)
                    .map_err(|e| anyhow!("read new_{}: {e}", st.dir()))?;
                write_extra(&args.out, &format!("new_{}", st.dir()), n, w, h, d)?;
            }
            if want_warpsrc {
                let (w, h, d) = inc
                    .read_warp_source()
                    .map_err(|e| anyhow!("read new_warpsrc: {e}"))?;
                write_extra(&args.out, "new_warpsrc", n, w, h, d)?;
            }
            if args.vars {
                let vars: BTreeMap<String, f64> =
                    engine.incoming_frame_vars().into_iter().collect();
                let dir = args.out.join("new_vars");
                std::fs::create_dir_all(&dir)?;
                std::fs::write(
                    dir.join(format!("{n:05}.json")),
                    lactea_compare::dump::vars_to_json(&vars),
                )?;
            }
            if transition_json.is_none()
                && let Some(t) = engine.transition_info()
            {
                transition_json = Some(serde_json::json!({
                    "shader": t.kind.index(), "name": t.kind.name(), "static": t.rand_static,
                    "start_time": t.start_time, "duration": t.duration,
                }));
            }
        }
        done += 1;
    }
    let secs = t0.elapsed().as_secs_f64();
    eprintln!(
        "fdrender: {done} frames in {secs:.2}s ({:.1} fps), {} validation error(s)",
        f64::from(done) / secs.max(1e-9),
        validation_errors.len()
    );
    for e in &validation_errors {
        eprintln!("fdrender: wgpu validation error: {e}");
    }

    let fallbacks2 = report2.as_ref().map_or(0, |r| r.shader_fallbacks.len());
    let mut info = serde_json::json!({
        "producer": PRODUCER,
        "preset_loaded": true,
        "preset_version": report.preset_version,
        "warp_shader": format!("{:?}", report.warp),
        "comp_shader": format!("{:?}", report.comp),
        "shader_fallbacks": report.shader_fallbacks.iter().map(|f| serde_json::json!({
            "pass": format!("{:?}", f.pass), "reason": f.reason, "fallback": f.fallback,
        })).collect::<Vec<_>>(),
        "unresolved_textures": report.unresolved_textures,
        "resolved_textures": report.resolved_textures,
        "warnings": report.warnings,
        "sprite": report.sprite,
        "preset2": report2.as_ref().map(|r| serde_json::json!({
            "preset_version": r.preset_version,
            "warp_shader": format!("{:?}", r.warp),
            "comp_shader": format!("{:?}", r.comp),
            "shader_fallbacks": r.shader_fallbacks.iter().map(|f| serde_json::json!({
                "pass": format!("{:?}", f.pass), "reason": f.reason, "fallback": f.fallback,
            })).collect::<Vec<_>>(),
            "unresolved_textures": r.unresolved_textures,
            "warnings": r.warnings,
        })),
        "transition": transition_json,
        "validation_errors": validation_errors,
        "frames_rendered": done,
        "teacher": teacher,
        "blend_teacher": args.blend_teacher.is_some(),
        "adapter": gpu.adapter_name,
        "seconds": secs,
    });
    // Only with the mode on and supported by the compat mode; the tool has no preset switching, so
    // this counts the cuts a host would have been asked for (mode 4 can ask twice in one frame).
    if engine.auto_hard_cut_active() {
        info["auto_cuts"] = serde_json::json!(auto_cuts);
    }
    info["compat"] = serde_json::json!(compat_name(report.compat));
    if auto_compat {
        info["compat_auto"] = serde_json::json!(true);
        info["compat_evidence"] = serde_json::json!(evidence);
        if let (Some(r), Some(ev)) = (&report2, &evidence2) {
            info["preset2"]["compat"] = serde_json::json!(compat_name(r.compat));
            info["preset2"]["compat_evidence"] = serde_json::json!(ev);
        }
    }
    write_info(&args.out, info)?;
    if !validation_errors.is_empty() {
        return Ok(4);
    }
    Ok(if report.shader_fallbacks.is_empty() && fallbacks2 == 0 {
        0
    } else {
        3
    })
}

fn compat_name(c: Compat) -> &'static str {
    match c {
        Compat::ProjectM => "projectm",
        Compat::MilkDrop2 => "md2",
        Compat::MilkDrop3 => "md3",
    }
}

/// What `lactea_preset::detect_compat` found in `bytes`, one string per piece of evidence, then the engine's
/// `fallback: ...` note when the load fell back to MilkDrop 2.
fn detected_evidence(bytes: &[u8], report: &lactea::PresetReport) -> Vec<String> {
    let mut ev: Vec<String> = lactea_preset::detect_compat(bytes)
        .evidence
        .iter()
        .map(ToString::to_string)
        .collect();
    ev.extend(
        report
            .warnings
            .iter()
            .filter(|w| w.starts_with("fallback: "))
            .cloned(),
    );
    ev
}

/// Name of the resolved mode and the evidence for the `--compat auto` log line.
fn detect_note(bytes: &[u8], report: &lactea::PresetReport) -> (&'static str, String) {
    let ev = detected_evidence(bytes, report);
    let ev = if ev.is_empty() {
        "none".to_string()
    } else {
        ev.join(", ")
    };
    (compat_name(report.compat), ev)
}

fn write_info(dir: &Path, v: serde_json::Value) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(
        dir.join("fdrender.json"),
        serde_json::to_string_pretty(&v)? + "\n",
    )?;
    Ok(())
}

fn main() {
    let args = match Args::try_parse() {
        Ok(a) => a,
        Err(e) => {
            // Exit 2 is reserved for preset-load failure; usage errors are 1, `--help`/`--version` 0.
            let code = if e.use_stderr() { 1 } else { 0 };
            let _ = e.print();
            std::process::exit(code);
        }
    };
    match validate(&args).and_then(|()| run(&args)) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("fdrender: {e:#}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(extra: &[&str]) -> Result<Args, clap::Error> {
        let mut v = vec![
            "fdrender", "--preset", "p.milk", "--audio", "a.wav", "--out", "o",
        ];
        v.extend_from_slice(extra);
        Args::try_parse_from(v)
    }

    #[test]
    fn bad_enum_flags_fail_in_clap() {
        assert!(parse(&["--compat", "nope"]).is_err());
        assert!(parse(&["--stroke", "nope"]).is_err());
        assert!(parse(&["--mouse", "2,2,up"]).is_err());
        assert!(parse(&["--mouse", "0.5,0.5,down"]).is_ok());
    }

    #[test]
    fn clap_usage_error_uses_stderr_help_does_not() {
        assert!(parse(&["--bogus"]).unwrap_err().use_stderr());
        let help = Args::try_parse_from(["fdrender", "--help"]).unwrap_err();
        assert!(!help.use_stderr());
    }

    #[test]
    fn compat_aliases() {
        for (s, c) in [
            ("projectm", Compat::ProjectM),
            ("md2", Compat::MilkDrop2),
            ("milkdrop2", Compat::MilkDrop2),
            ("md3", Compat::MilkDrop3),
            ("milkdrop3", Compat::MilkDrop3),
        ] {
            assert_eq!(
                parse(&["--compat", s]).unwrap().compat,
                CompatChoice::Fixed(c)
            );
        }
    }

    #[test]
    fn compat_auto() {
        assert_eq!(
            parse(&["--compat", "auto"]).unwrap().compat,
            CompatChoice::Auto
        );
    }

    #[test]
    fn size_accepts_uppercase_x() {
        assert_eq!(parse_size("640X360").unwrap(), (640, 360));
        assert_eq!(parse_size("640x360").unwrap(), (640, 360));
        assert!(parse_size("640").is_err());
    }

    #[test]
    fn missing_texture_dir_is_an_error() {
        let a = parse(&["--texture-dir", "/nonexistent/lactea-tex"]).unwrap();
        assert!(validate(&a).is_err());
    }

    #[test]
    fn switch_frame_must_be_inside_the_run() {
        let a = parse(&[
            "--preset2",
            "q.milk",
            "--switch-frame",
            "10",
            "--frames",
            "2",
        ])
        .unwrap();
        assert!(validate(&a).is_err());
        let a = parse(&[
            "--preset2",
            "q.milk",
            "--switch-frame",
            "1",
            "--frames",
            "2",
        ])
        .unwrap();
        assert!(validate(&a).is_ok());
    }

    #[test]
    fn transition_flags_need_preset2() {
        for flag in [
            &["--switch-frame", "1"][..],
            &["--soft-cut", "1"],
            &["--start-clean", "1"],
            &["--transition-shader", "1"],
            &["--transition-static", "1,2,3,4"],
        ] {
            assert!(validate(&parse(flag).unwrap()).is_err(), "{flag:?}");
        }
    }
}
