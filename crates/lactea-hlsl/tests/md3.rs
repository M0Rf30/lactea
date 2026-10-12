//! MilkDrop 3 shader inputs (`Options::md3`): the prelude (`_c14`/`mouse*`, `q33..q64`, `sampler_fft`, `get_fft*`),
//! the FFT lookup lowering, the legacy comp generator variants, and the regressions the MD3 shaders exposed
//! (a local shadowing a macro-defined global).
mod common;

use common::gpu::{Gpu, fft_texels, make_ext_uniforms};
use common::*;
use lactea_abi::*;
use lactea_hlsl::*;

fn md3() -> Options {
    let mut o = Options::default();
    o.md3 = true;
    o
}

fn src(globals: &str, body: &str) -> String {
    format!("{globals}\nshader_body\n{{\n{body}\n}}\n")
}

/// Translate with `opts`, check the WGSL with naga, return it.
fn tr_with(kind: PassKind, globals: &str, body: &str, opts: &Options) -> Translated {
    let t = translate(kind, &src(globals, body), opts).unwrap_or_else(|e| panic!("{e}\n{body}"));
    naga_validate(&t.wgsl).unwrap_or_else(|e| panic!("{e}\n{}", t.wgsl));
    t
}

fn tr3(body: &str) -> Translated {
    tr_with(PassKind::Warp, "", body, &md3())
}

fn fft_bindings(t: &Translated) -> Vec<&TextureBinding> {
    t.interface
        .textures
        .iter()
        .filter(|b| b.source == TexSource::Fft)
        .collect()
}

// ---------------------------------------------------------------- prelude text

#[test]
fn prelude_sits_between_include_and_pass_defines() {
    let w = md3_wrapped_hlsl(
        PassKind::Warp,
        "float4 mine;\nshader_body\n{\n ret = 1;\n}\n",
    )
    .unwrap();
    let i_inc = w.find("float4   rand_frame").unwrap();
    let i_c14 = w.find("float4 _c14;").unwrap();
    let i_def = w.find("#define uv_orig _uv.zw").unwrap();
    let i_usr = w.find("float4 mine;").unwrap();
    assert!(i_inc < i_c14 && i_c14 < i_def && i_def < i_usr);
    // the registers and the 32 macros: qN is component (N-1)%4 of register i + (N-33)/4
    for n in 33..=64usize {
        let reg = (b'i' + ((n - 33) / 4) as u8) as char;
        let comp = ["x", "y", "z", "w"][(n - 1) % 4];
        assert!(
            w.contains(&format!("#define q{n} _q{reg}.{comp}\n")),
            "q{n}"
        );
    }
    assert!(w.contains("#define q33 _qi.x\n") && w.contains("#define q64 _qp.w\n"));
    assert!(w.contains("float4 _qi, _qj, _qk, _ql, _qm, _qn, _qo, _qp;"));
    for m in [
        "#define mouse _c14\n",
        "#define mouse_x _c14.x\n",
        "#define mouse_y _c14.y\n",
        "#define mouse_z _c14.z\n",
        "#define mouse_w _c14.w\n",
        "#define mouse_pos _c14.xy\n",
        "sampler2D sampler_fft;\n",
        "#define get_fft(pos) tex2D(sampler_fft, float2(clamp(pos, 0, 1), 0.5)).r\n",
        "#define get_fft_hz(freq) get_fft(clamp((freq) / 24000.0, 0, 1))\n",
    ] {
        assert!(w.contains(m), "{m}");
    }
    // the audio-envelope / beat-latch uniforms and their component macros
    assert!(
        w.contains("float4 _mxd3qxa, _mxd3qxb, _mxd3qxc, _mxd3qxd, _mxd3qsa, _mxd3qsb, _mxd3qsc;")
    );
    for m in [
        "#define MXD3QX1 _mxd3qxa.x\n",
        "#define MXD3QX4 _mxd3qxa.w\n",
        "#define MXD3QX5 _mxd3qxb.x\n",
        "#define MXD3QX16 _mxd3qxd.w\n",
        "#define MXD3QS1 _mxd3qsa.x\n",
        "#define MXD3QS4 _mxd3qsa.w\n",
        "#define MXD3QS5 _mxd3qsb.x\n",
        "#define MXD3QS12 _mxd3qsc.w\n",
    ] {
        assert!(w.contains(m), "{m}");
    }
    // BeatDrop-only names are not declared
    for absent in [
        "get_fft_peak",
        "get_wave",
        "sampler_wave",
        "HAS_FFT_PEAK",
        "_c15",
    ] {
        assert!(!w.contains(absent), "{absent}");
    }
}

#[test]
fn md2_wrapper_is_untouched_by_md3() {
    let w = md2_wrapped_hlsl(PassKind::Warp, "shader_body\n{\n ret = 1;\n}\n").unwrap();
    for absent in [
        "_c14",
        "_qi",
        "mouse",
        "q33",
        "sampler_fft",
        "get_fft",
        "texFFT",
    ] {
        assert!(!w.contains(absent), "{absent}");
    }
    // md3_wrapped_hlsl is md2_wrapped_hlsl plus exactly the prelude block
    let w3 = md3_wrapped_hlsl(PassKind::Warp, "shader_body\n{\n ret = 1;\n}\n").unwrap();
    let at = w3.find("float4 _c14;").unwrap();
    let end = w3.find("#define rad _rad_ang.x").unwrap();
    assert_eq!(format!("{}{}", &w3[..at], &w3[end..]), w);
}

#[test]
fn prelude_is_absent_without_md3() {
    // `get_fft` is an unknown function, `mouse` / `q33` unknown identifiers: exactly as before MD3 support
    let e = translate(
        PassKind::Warp,
        &src("", "ret = get_fft(0.5);"),
        &Options::default(),
    )
    .unwrap_err();
    assert_eq!(e.category(), "unsupported", "{e}");
    assert!(e.message().contains("unknown function"), "{e}");
    for body in ["ret = mouse.xyx;", "ret = q33;", "ret = mouse_x;"] {
        let e = translate(PassKind::Warp, &src("", body), &Options::default()).unwrap_err();
        assert_eq!(e.category(), "type", "{body}: {e}");
    }
    // `_c14` and `_qi` are ordinary (private, zero) globals when a shader declares them itself
    let t = tr_with(
        PassKind::Warp,
        "float4 _c14; float4 _qi;",
        "ret = _c14.xyz + _qi.xyz;",
        &Options::default(),
    );
    assert!(!t.wgsl.contains("mx.mouse") && !t.wgsl.contains("mx.q_hi"));
}

// ---------------------------------------------------------------- scoping regression

#[test]
fn local_shadowing_a_macro_global_sees_the_outer_variable() {
    // `aspect` expands to `_c0`: `float _c0 = _c0.x / _c0.y;` - the initialiser reads the outer float4 `_c0`,
    // everything after the declaration sees the new scalar. MD2 accepts this, so it must work with and without md3.
    let body = "float aspect = aspect.x / aspect.y; ret = tex2D(sampler_main, uv * aspect).xyz;";
    for opts in [Options::default(), md3()] {
        let t = tr_with(PassKind::Warp, "", body, &opts);
        assert!(t.wgsl.contains("mu.c[0].x / mu.c[0].y"), "{}", t.wgsl);
    }
    // the same through a comp shader, with a vector shadow and a second use of the macro afterwards
    let body = "float2 aspect = aspect.xy * 2; ret = tex2D(sampler_main, uv * aspect).xyz;";
    tr_with(PassKind::Comp, "", body, &md3());
    tr_with(PassKind::Comp, "", body, &Options::default());
    // and a shadow inside a nested block does not leak out of it
    let body = "{ float aspect = aspect.x; ret.x = aspect; } ret.y = aspect.y; ret.z = 0;";
    tr_with(PassKind::Warp, "", body, &md3());
    tr_with(PassKind::Warp, "", body, &Options::default());
    // writing to the shadowing local makes the (never written) outer global a seeded private copy, nothing breaks
    let body = "float aspect = aspect.x / aspect.y; aspect *= 2; ret = tex2D(sampler_main, uv * aspect).xyz;";
    for opts in [Options::default(), md3()] {
        let t = tr_with(PassKind::Warp, "", body, &opts);
        assert!(t.wgsl.contains("g_c0.x / g_c0.y"), "{}", t.wgsl);
    }
    // `mouse` (= `_c14`) is shadowed the same way, also through the uniform-write path; the component macros
    // (`q40` = `_qj.w`) expand to a swizzle, so a declaration named after one is a syntax error in HLSL too
    let body = "float2 mouse = mouse.xy * 2; mouse += q40; ret = float3(mouse, q41);";
    let t = tr3(body);
    assert!(t.wgsl.contains("g_c14.xy * 2.0f"), "{}", t.wgsl);
    // a function parameter and a loop variable named after a macro, with an initialiser using the global
    let body =
        "for (float aspect = aspect.x; aspect < 3; aspect += 1) { ret.x += aspect; } ret.yz = 0;";
    tr_with(PassKind::Warp, "", body, &md3());
    tr_with(PassKind::Warp, "", body, &Options::default());
    let g = "float f(float aspect) { return aspect * 2; }";
    tr_with(PassKind::Warp, g, "ret = f(aspect.x).xxx;", &md3());
    // projectM flavour (undefined-variable analysis) takes the same path
    let mut pm = md3();
    pm.projectm_flavor = true;
    tr_with(
        PassKind::Warp,
        "",
        "float aspect = aspect.x / aspect.y; ret = tex2D(sampler_main, uv * aspect).xyz;",
        &pm,
    );
}

// ---------------------------------------------------------------- mouse and q33..q64

#[test]
fn mouse_names_map_to_the_extension_uniform() {
    let t = tr3(
        "float2 p = mouse.xy; float2 q = mouse_pos; ret = float3(p + q, mouse_x + mouse_y * mouse_z - mouse.w);",
    );
    assert!(t.wgsl.contains("mx.mouse"), "{}", t.wgsl);
    // never a private zero global, never an out-of-range `mu.c[14]`
    assert!(!t.wgsl.contains("mu.c[14]") && !t.wgsl.contains("g_c14"));
    // each spelling reads its component
    let t = tr3("ret = float3(mouse_x, mouse_y, mouse_z) + mouse_w + mouse.yzw;");
    for c in ["mx.mouse.x", "mx.mouse.y", "mx.mouse.z", "mx.mouse.w"] {
        assert!(t.wgsl.contains(c), "{c}");
    }
    // and in a comp shader
    tr_with(
        PassKind::Comp,
        "",
        "ret = tex2D(sampler_main, mouse.xy).xyz;",
        &md3(),
    );
    // a shader may write to the uniform: private copy seeded from it, like `_qa`
    let t = tr3("mouse.x = 0.5; mouse_y += 0.25; ret = float3(mouse.xy, mouse_z);");
    assert!(t.wgsl.contains("g_c14 = mx.mouse;"), "{}", t.wgsl);
}

#[test]
fn q33_to_q64_map_to_q_hi() {
    let t = tr3("ret = float3(q33, q64, q40);");
    assert!(t.wgsl.contains("mx.q_hi[0].x"), "{}", t.wgsl);
    assert!(t.wgsl.contains("mx.q_hi[7].w"));
    assert!(t.wgsl.contains("mx.q_hi[1].w")); // q40 = _qj.w
    // the frozen block still serves q1..q32, also next to the new ones
    let t = tr3("ret = float3(q1 + q32, q33 + q36, q37 + q61);");
    for c in [
        "mu.q[0].x",
        "mu.q[7].w",
        "mx.q_hi[0].x",
        "mx.q_hi[0].w",
        "mx.q_hi[1].x",
        "mx.q_hi[7].x",
    ] {
        assert!(t.wgsl.contains(c), "{c}");
    }
    // every name maps onto register (N-1)/4 component (N-1)%4
    for n in 33..=64usize {
        let t = tr3(&format!("ret = q{n}.xxx;"));
        let want = format!(
            "mx.q_hi[{}].{}",
            (n - 33) / 4,
            ["x", "y", "z", "w"][(n - 1) % 4]
        );
        assert!(t.wgsl.contains(&want), "q{n}: {want}\n{}", t.wgsl);
    }
    // writing one works: private copy of its register, seeded from the uniform
    let t = tr3("q40 = 1; ret = float3(q40, q41, q33);");
    assert!(t.wgsl.contains("g_qj = mx.q_hi[1];"), "{}", t.wgsl);
    // the untouched registers stay direct reads
    assert!(t.wgsl.contains("mx.q_hi[0].x"));
    // q1..q32 behave exactly like before
    let t = tr3("q5 = 2; ret = q5.xxx;");
    assert!(t.wgsl.contains("g_qb = mu.q[1];"));
}

// ---------------------------------------------------------------- get_fft and the FFT sampler

#[test]
fn get_fft_binds_the_fft_texture_with_clamp_and_bilinear() {
    let t = tr3("ret = get_fft(uv.x) + get_fft_hz(440) + get_fft(bass * 0.5 + 0.25);");
    let f = fft_bindings(&t);
    assert_eq!(f.len(), 1, "{:?}", t.interface.textures);
    assert_eq!(f[0].hlsl_name, "sampler_fft");
    assert_eq!(
        (f[0].filter, f[0].address),
        (Filter::Linear, Address::Clamp)
    );
    assert_eq!(f[0].dim, TexDim::D2);
    // the manual lookup replaced textureSample for it; the helper is in the module exactly once
    assert!(t.wgsl.contains("fn fft_fetch("), "{}", t.wgsl);
    assert_eq!(t.wgsl.matches("fn fft_fetch(").count(), 1);
    assert_eq!(t.wgsl.matches("fn fft_texel(").count(), 1);
    assert_eq!(t.wgsl.matches("fft_fetch(tex_sampler_fft, ").count(), 3);
    assert!(!t.wgsl.contains("textureSample("), "{}", t.wgsl);
    // the usual texture + sampler pair is declared for the binding (the renderer binds an R32Float view)
    assert!(t.wgsl.contains(&format!(
        "@group(1) @binding({}) var tex_sampler_fft: texture_2d<f32>;",
        texture_binding(0)
    )));
    assert!(t.wgsl.contains(&format!(
        "@group(1) @binding({}) var smp_sampler_fft: sampler;",
        sampler_binding(0)
    )));
    // the texture is declared after include.fx's sampler_blur3 only if it is used; here it is the only binding
    assert_eq!(t.interface.textures.len(), 1);
    assert_eq!(t.interface.max_blur_level, 0);
}

#[test]
fn fft_helper_is_only_emitted_when_an_fft_binding_exists() {
    let t = tr3("ret = tex2D(sampler_main, uv).xyz;");
    assert!(!t.wgsl.contains("fft_fetch") && !t.wgsl.contains("FFT_BINS"));
    // md3 off: `sampler_fft` is the BeatDrop-style user texture `fft`, sampled normally, no helper
    let t = tr_with(
        PassKind::Warp,
        "sampler2D sampler_fft;",
        "ret = tex2D(sampler_fft, uv).xyz;",
        &Options::default(),
    );
    assert_eq!(t.interface.textures.len(), 1);
    assert_eq!(
        t.interface.textures[0].source,
        TexSource::User("fft".into())
    );
    assert!(t.wgsl.contains("textureSample(tex_sampler_fft"));
    assert!(!t.wgsl.contains("fft_fetch"));
    // implicit declaration (`sampler_fft` without md3 is declared for the shader like any sampler_<name>)
    let t = tr_with(
        PassKind::Warp,
        "",
        "ret = tex2D(sampler_fft, uv).xyz;",
        &Options::default(),
    );
    assert_eq!(
        t.interface.textures[0].source,
        TexSource::User("fft".into())
    );
}

#[test]
fn fft_sampler_prefixes_select_filter_and_wrap() {
    let t = tr3(
        "ret = tex2D(sampler_pw_fft, uv).xxx + tex2D(sampler_fw_fft, uv).xxx + tex2D(sampler_pc_fft, uv).xxx;",
    );
    let f = fft_bindings(&t);
    let mut got: Vec<_> = f
        .iter()
        .map(|b| (b.hlsl_name.as_str(), b.filter, b.address))
        .collect();
    got.sort_by_key(|g| g.0);
    assert_eq!(
        got,
        [
            ("sampler_fw_fft", Filter::Linear, Address::Wrap),
            ("sampler_pc_fft", Filter::Point, Address::Clamp),
            ("sampler_pw_fft", Filter::Point, Address::Wrap),
        ]
    );
    // one helper, three calls with the mode baked in
    assert_eq!(t.wgsl.matches("fn fft_fetch(").count(), 1);
    assert!(t.wgsl.contains("fft_fetch(tex_sampler_pw_fft, ") && t.wgsl.contains(", false, true)"));
    assert!(t.wgsl.contains("fft_fetch(tex_sampler_fw_fft, ") && t.wgsl.contains(", true, true)"));
    assert!(
        t.wgsl.contains("fft_fetch(tex_sampler_pc_fft, ") && t.wgsl.contains(", false, false)")
    );
}

#[test]
fn every_2d_lookup_flavour_of_the_fft_sampler_is_lowered() {
    let t = tr3("float4 a = tex2D(sampler_fft, uv);
         float4 b = tex2Dlod(sampler_fft, float4(0.3, 0.5, 0, 2));
         float4 c = tex2Dbias(sampler_fft, float4(0.3, 0.5, 0, 1));
         float4 d = tex2Dproj(sampler_fft, float4(0.6, 1, 0, 2));
         float4 e = tex2Dgrad(sampler_fft, float2(0.3, 0.5), float2(0.1, 0), float2(0, 0.1));
         ret = a.xyz + b.xyz + c.xyz + d.xyz + e.xyz;");
    assert_eq!(t.wgsl.matches("fft_fetch(tex_sampler_fft, ").count(), 5);
    for s in [
        "textureSample",
        "textureSampleLevel",
        "textureSampleBias",
        "textureSampleGrad",
    ] {
        assert!(!t.wgsl.contains(s), "{s}\n{}", t.wgsl);
    }
    // a 3D lookup of a 2D texture is an error, not a miscompile
    let e = translate(
        PassKind::Warp,
        &src("", "ret = tex3D(sampler_fft, float3(uv, 0)).xyz;"),
        &md3(),
    )
    .unwrap_err();
    assert!(e.message().contains("sampler_fft"), "{e}");
}

#[test]
fn texsize_fft_and_wave_under_md3() {
    let t = tr_with(
        PassKind::Warp,
        "float4 texsize_fft; sampler2D sampler_wave; float4 texsize_wave;",
        "ret = tex2D(sampler_fft, uv * texsize_fft.zw).xxx + tex2D(sampler_wave, uv).xyz * texsize_wave.zwx;",
        &md3(),
    );
    // `wave` is an ordinary user texture (MD3 never fills it: the engine binds its black dummy)
    let wave = t
        .interface
        .textures
        .iter()
        .find(|b| b.hlsl_name == "sampler_wave")
        .unwrap();
    assert_eq!(wave.source, TexSource::User("wave".into()));
    assert_eq!(
        (wave.filter, wave.address),
        (Filter::Linear, Address::Clamp)
    );
    assert_eq!(t.texsize_params, ["texsize_fft", "texsize_wave"]);
    assert_eq!(
        t.interface.texsize_slots,
        [TexSource::Fft, TexSource::User("wave".into())]
    );
    // BeatDrop's helpers do not exist in MD3
    for f in ["get_fft_peak(0.5)", "get_wave(0.5)", "get_fft_peak_hz(100)"] {
        let e = translate(PassKind::Warp, &src("", &format!("ret = {f};")), &md3()).unwrap_err();
        assert!(e.message().contains("unknown function"), "{f}: {e}");
    }
}

#[test]
fn macro_arguments_are_substituted_textually() {
    // the argument is pasted into `clamp(pos, 0, 1)`: a ternary, a nested macro and a function call all work
    let t = tr3(
        "ret = get_fft(bass > 0.5 ? 0.25 : 0.75) + get_fft(mouse_x) + get_fft(abs(sin(time))) + get_fft_hz(bass * 1000 + 40);",
    );
    assert_eq!(t.wgsl.matches("fft_fetch(tex_sampler_fft, ").count(), 4);
    // the argument text lands inside the clamp, nested macros (`mouse_x`, `bass`) expanded in place
    for part in [
        "clamp(select(0.75f, 0.25f, (mu.c[3].x > 0.5f)), 0.0f, 1.0f)",
        "clamp(mx.mouse.x, 0.0f, 1.0f)",
        "clamp(abs(sin(mu.c[2].x)), 0.0f, 1.0f)",
        // get_fft_hz = get_fft(clamp((f) / 24000.0, 0, 1)): two clamps, the argument divided by 24000 first
        "clamp(clamp((((mu.c[3].x * 1000.0f) + 40.0f) / 24000.0f), 0.0f, 1.0f), 0.0f, 1.0f)",
    ] {
        assert!(t.wgsl.contains(part), "{part}\n{}", t.wgsl);
    }
}

#[test]
fn md3_translation_of_existing_shaders_only_adds_unused_declarations() {
    // Without the fft sampler in use, an md3 translation of an MD2 shader equals the MD2 translation
    // (the prelude declares names, it binds nothing by itself).
    let body = "float2 d = uv - 0.5; ret = tex2D(sampler_main, uv + d * q1 * 0.01).xyz * (0.9 + 0.1 * bass) + GetBlur1(uv);";
    let a = translate(PassKind::Warp, &src("", body), &Options::default()).unwrap();
    let b = translate(PassKind::Warp, &src("", body), &md3()).unwrap();
    assert_eq!(a.wgsl, b.wgsl);
    assert_eq!(a.interface, b.interface);
    assert_eq!(a.texsize_params, b.texsize_params);
}

#[test]
fn corpus_shaders_translate_identically_under_md3() {
    // The prelude must not disturb MD2 shaders: same output, same interface, same errors for every corpus shader.
    let cases = corpus_cases();
    if cases.is_empty() {
        eprintln!("corpus not present, skipping");
        return;
    }
    let results = par_map(&cases, |c| {
        (
            translate(c.kind, &c.text, &Options::default()),
            translate(c.kind, &c.text, &md3()),
        )
    });
    let mut diff = Vec::new();
    for (c, (a, b)) in cases.iter().zip(&results) {
        let same = match (a, b) {
            (Ok(a), Ok(b)) => {
                a.wgsl == b.wgsl
                    && a.interface == b.interface
                    && a.texsize_params == b.texsize_params
            }
            (Err(a), Err(b)) => a == b,
            _ => false,
        };
        if !same {
            diff.push(c.label());
        }
    }
    assert!(
        diff.is_empty(),
        "{} shaders differ:\n{}",
        diff.len(),
        diff.join("\n")
    );
}

// ---------------------------------------------------------------- legacy comp generator

#[test]
fn legacy_comp_generator_md3_effects() {
    let mk = |md3_effects| {
        default_comp_shader(1.0, 0.0, 1.0, 0, 0.0, true, false, true, false, md3_effects)
    };
    let md2 = mk(false);
    assert!(md2.contains("ret = 1 - (1-ret) * (1-ret); //brighten"));
    assert!(md2.contains("ret = ret*(1-ret)*2; //solarize"));
    let md3t = mk(true);
    assert!(md3t.contains("ret = sqrt(ret); //brighten\n"), "{md3t}");
    assert!(md3t.contains("ret = ret*(1-ret)*4; //solarize\n"), "{md3t}");
    assert!(!md3t.contains("1 - (1-ret)") && !md3t.contains("*2; //solarize"));
    // the rest of the text is identical, and the effect lines are the only difference
    let strip = |s: &str| {
        s.lines()
            .filter(|l| !l.contains("//brighten") && !l.contains("//solarize"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(strip(&md2), strip(&md3t));
    // without the effects the flag changes nothing
    assert_eq!(
        default_comp_shader(1.2, 0.5, 1.25, 1, 0.3, false, true, false, true, false),
        default_comp_shader(1.2, 0.5, 1.25, 1, 0.3, false, true, false, true, true)
    );
    // both translate under their own dialect
    translate(PassKind::Comp, &md2, &Options::default()).unwrap();
    let t = tr_with(
        PassKind::Comp,
        "",
        &md3t.replace("shader_body\n{\n", "").replace("}\n", ""),
        &md3(),
    );
    assert!(t.wgsl.contains("sqrt(abs("), "{}", t.wgsl);
}

// ---------------------------------------------------------------- GPU: the lowering reproduces D3D9 R32F

/// D3D9 semantics of an R32F lookup on the `FFT_BINS x 1` texture (the model of the translator's helper).
fn fft_model(s: &[f32], u: f32, linear: bool, wrap: bool) -> f32 {
    let n = FFT_BINS as i32;
    let idx = |i: i32| -> usize {
        (if wrap {
            i.rem_euclid(n)
        } else {
            i.clamp(0, n - 1)
        }) as usize
    };
    if linear {
        let x = u * n as f32 - 0.5;
        let f = x.floor();
        let (a, b) = (s[idx(f as i32)], s[idx(f as i32 + 1)]);
        a + (b - a) * (x - f)
    } else {
        s[idx((u * n as f32).floor() as i32)]
    }
}

fn render(gpu: &Gpu, t: &Translated, kind: PassKind) -> Vec<u8> {
    let d = &gpu.device;
    let sv = d.push_error_scope(wgpu::ErrorFilter::Validation);
    let module = d.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(t.wgsl.clone().into()),
    });
    let px = gpu.render(
        kind,
        &module,
        "fs_main",
        false,
        &t.interface,
        &t.interface.texsize_slots,
    );
    if let Some(e) = pollster::block_on(sv.pop()) {
        panic!("GPU validation error: {e}\n{}", t.wgsl);
    }
    px
}

/// Channel `c` of the pixel at column `x`, row `y` as 0..255.
fn at(px: &[u8], x: usize, y: usize, c: usize) -> f32 {
    px[(y * 64 + x) * 4 + c] as f32
}

fn gpu_or_skip() -> Option<Gpu> {
    let g = Gpu::new();
    if g.is_none() {
        eprintln!("no GPU adapter, skipping");
    }
    g
}

#[test]
fn gpu_get_fft_matches_the_d3d9_model() {
    let Some(gpu) = gpu_or_skip() else { return };
    let s = fft_texels();
    // fixed points of the spec: get_fft(0.5) averages S[255] and S[256], get_fft(0) = S[0], get_fft(1) = S[511],
    // get_fft(-3) = S[0], get_fft(7) = S[511]
    let t = tr3("ret = float3(get_fft(0.5), get_fft(0), get_fft(1));");
    let px = render(&gpu, &t, PassKind::Warp);
    let want = [(s[255] + s[256]) / 2.0, s[0], s[511]];
    for (c, w) in want.iter().enumerate() {
        assert!(
            (at(&px, 20, 30, c) - w * 255.0).abs() <= 1.0,
            "channel {c}: {} vs {}",
            at(&px, 20, 30, c),
            w * 255.0
        );
    }
    assert!((want[0] - 0.715).abs() < 1e-6 && want[0] != s[255]);
    let t = tr3("ret = float3(get_fft(-3), get_fft(7), get_fft_hz(24000));");
    let px = render(&gpu, &t, PassKind::Warp);
    for (c, w) in [s[0], s[511], s[511]].iter().enumerate() {
        assert!((at(&px, 5, 5, c) - w * 255.0).abs() <= 1.0, "channel {c}");
    }
    // get_fft_hz(f) = get_fft(f / 24000)
    let t = tr3("ret = float3(get_fft_hz(1200), get_fft_hz(12000), get_fft_hz(30000));");
    let px = render(&gpu, &t, PassKind::Warp);
    let want = [
        fft_model(&s, 1200.0 / 24000.0, true, false),
        fft_model(&s, 0.5, true, false),
        s[511],
    ];
    for (c, w) in want.iter().enumerate() {
        assert!(
            (at(&px, 9, 9, c) - w * 255.0).abs() <= 1.0,
            "hz channel {c}"
        );
    }
    // a ramp over the screen: every column against the model (uv_orig.x is (x + 0.5) / 64 at the pixel centre)
    let t = tr3(
        "float u = uv_orig.x * 0.9 + 0.013; ret = float3(get_fft(u), get_fft(u * 5 - 2), get_fft(1 - u * 0.5));",
    );
    let px = render(&gpu, &t, PassKind::Warp);
    for x in 0..64 {
        let u = (x as f32 + 0.5) / 64.0 * 0.9 + 0.013;
        let want = [
            fft_model(&s, u.clamp(0.0, 1.0), true, false),
            fft_model(&s, (u * 5.0 - 2.0).clamp(0.0, 1.0), true, false),
            fft_model(&s, (1.0 - u * 0.5).clamp(0.0, 1.0), true, false),
        ];
        for (c, w) in want.iter().enumerate() {
            let got = at(&px, x, 40, c);
            assert!(
                (got - w * 255.0).abs() <= 2.0,
                "x={x} ch={c}: {got} vs {}",
                w * 255.0
            );
        }
    }
}

#[test]
fn gpu_filter_and_wrap_modes_follow_the_sampler_prefixes() {
    let Some(gpu) = gpu_or_skip() else { return };
    let s = fft_texels();
    // u runs over (-0.35 .. 1.55): clamp for the plain/fc/pc samplers, wrap for fw/pw
    let t = tr3("float u = uv_orig.x * 1.9 - 0.35;
         ret = float3(tex2D(sampler_fw_fft, float2(u, 0.1)).r,
                      tex2D(sampler_pw_fft, float2(u, 0.9)).r,
                      tex2D(sampler_pc_fft, float2(u, 0.5)).r);");
    let px = render(&gpu, &t, PassKind::Warp);
    for x in 0..64 {
        let u = (x as f32 + 0.5) / 64.0 * 1.9 - 0.35;
        let want = [
            fft_model(&s, u, true, true),
            fft_model(&s, u, false, true),
            fft_model(&s, u, false, false),
        ];
        for (c, w) in want.iter().enumerate() {
            let got = at(&px, x, 20, c);
            assert!(
                (got - w * 255.0).abs() <= 2.0,
                "x={x} ch={c} u={u}: {got} vs {}",
                w * 255.0
            );
        }
    }
    // D3D9 R32F reads (r, 1, 1, 1); v is ignored; lod/bias/proj/grad read the same texel
    let t = tr3(
        "float4 a = tex2D(sampler_fft, float2(0.3, 0.1));
         float4 b = tex2Dlod(sampler_fft, float4(0.3, 0.9, 0, 3));
         float4 c = tex2Dproj(sampler_fft, float4(0.6, 1, 0, 2));
         ret = float3(a.r, abs(a.r - b.r) + abs(a.r - c.r) + abs(a.r - tex2Dbias(sampler_fft, float4(0.3, 0.2, 0, 1)).r), a.g * a.b * a.a);",
    );
    let px = render(&gpu, &t, PassKind::Warp);
    let w = fft_model(&s, 0.3, true, false);
    assert!((at(&px, 3, 3, 0) - w * 255.0).abs() <= 1.0);
    assert_eq!(
        at(&px, 3, 3, 1),
        0.0,
        "lod/bias/proj/grad must agree with tex2D"
    );
    assert_eq!(at(&px, 3, 3, 2), 255.0, "gba of an R32F texture is 1");
}

#[test]
fn gpu_mouse_and_q_hi_are_read_from_the_extension_block() {
    let Some(gpu) = gpu_or_skip() else { return };
    let mx = make_ext_uniforms();
    assert_eq!((mx.mouse[1], mx.mouse[2]), (0.75, 1.0));
    let q = |n: usize| mx.q_hi[(n - 33) / 4][(n - 1) % 4];
    let t = tr3("ret = float3(q33 * 16, q64, mouse_y);");
    let px = render(&gpu, &t, PassKind::Warp);
    for (c, w) in [q(33) * 16.0, q(64), mx.mouse[1]].iter().enumerate() {
        assert!((at(&px, 7, 7, c) - w * 255.0).abs() <= 1.0, "channel {c}");
    }
    // a write goes to a private copy: later reads see it, neighbours of the same register keep the uniform
    let t = tr3("q40 = q40 * 0.5; ret = float3(q40, q41, mouse_z * q33 * 16);");
    let px = render(&gpu, &t, PassKind::Warp);
    for (c, w) in [q(40) * 0.5, q(41), mx.mouse[2] * q(33) * 16.0]
        .iter()
        .enumerate()
    {
        assert!(
            (at(&px, 11, 2, c) - w * 255.0).abs() <= 1.0,
            "write channel {c}"
        );
    }
}
