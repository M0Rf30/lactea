//! Unit tests: MD2 text wrapping, default shaders, sampler rules, error handling, robustness.
mod common;

use common::*;
use lactea_abi::*;
use lactea_hlsl::*;

fn tr(kind: PassKind, body: &str) -> Translated {
    trg(kind, "", body)
}

fn trg(kind: PassKind, globals: &str, body: &str) -> Translated {
    let src = format!("{globals}\nshader_body\n{{\n{body}\n}}\n");
    let t = translate(kind, &src, &Options::default()).unwrap_or_else(|e| panic!("{e}\n{body}"));
    naga_validate(&t.wgsl).unwrap_or_else(|e| panic!("{e}\n{}", t.wgsl));
    t
}

#[test]
fn wrapper_follows_load_shader_from_memory() {
    let w = md2_wrapped_hlsl(
        PassKind::Warp,
        "sampler foo;\nshader_body\n{\n  ret = 1;\n}\ntrailing junk",
    )
    .unwrap();
    // include.fx is pasted first, then the warp defines, then the user text
    let i_inc = w.find("float4   rand_frame").unwrap();
    let i_def = w.find("#define uv_orig _uv.zw").unwrap();
    let i_usr = w.find("sampler foo;").unwrap();
    assert!(i_inc < i_def && i_def < i_usr);
    // `shader_body` becomes the PS header, `float3 ret = 0;` is injected after the first `{`
    assert!(w.contains("void PS( float4 _vDiffuse : COLOR, float4 _uv : TEXCOORD0, float2 _rad_ang : TEXCOORD1, out float4 _return_value : COLOR0 )"));
    assert!(w.contains("{    float3 ret = 0;\n"));
    // everything from the last `}` on is replaced by the return statement
    assert!(w.contains("_return_value = float4(ret.xyz, _vDiffuse.w);\n}\n"));
    assert!(!w.contains("trailing junk"));
    // composite: uv_orig is uv (MD2 [sic]), hue_shader = _vDiffuse.xyz, _uv is a float2
    let c = md2_wrapped_hlsl(PassKind::Comp, "shader_body { ret = hue_shader; }").unwrap();
    assert!(c.contains("#define uv_orig _uv.xy") && c.contains("#define hue_shader _vDiffuse.xyz"));
    assert!(c.contains("float2 _uv : TEXCOORD0"));
}

#[test]
fn wrapper_errors_like_md2() {
    assert!(matches!(
        md2_wrapped_hlsl(PassKind::Warp, "float4 x;"),
        Err(TranslateError::Wrap(_))
    ));
    assert!(matches!(
        md2_wrapped_hlsl(PassKind::Warp, "shader_body"),
        Err(TranslateError::Wrap(_))
    ));
    assert!(matches!(
        md2_wrapped_hlsl(PassKind::Warp, "shader_body {"),
        Err(TranslateError::Wrap(_))
    ));
}

#[test]
fn default_shader_texts() {
    let w = default_warp_shader(0.98, true);
    assert!(w.contains("ret = tex2D( sampler_main, uv ).xyz;") && w.contains("ret *= 0.98;"));
    assert!(default_warp_shader(0.9, false).contains("sampler_fc_main"));
    let c = default_comp_shader(1.0, 0.0, 1.0, 0, 0.0, false, false, false, false);
    assert!(c.contains("ret = tex2D(sampler_main, uv).xyz;") && c.contains("ret *= 1.00; //gamma"));
    let c = default_comp_shader(1.5, 0.5, 2.0, 3, 0.5, true, true, true, true);
    assert!(c.contains("float2 uv_echo = (uv - 0.5)*0.500*float2(-1,-1) + 0.5;"));
    assert!(c.contains("ret *= 0.50 + 0.50*hue_shader;") && c.contains("//invert"));
    // all variants translate
    for (a, d) in [(0.0, 0.0), (0.4, 1.0), (0.4, 0.3)] {
        let txt = default_comp_shader(1.2, a, 1.1, 1, d, true, false, true, false);
        translate(PassKind::Comp, &txt, &Options::default()).unwrap();
    }
    for wrap in [true, false] {
        let t = translate(
            PassKind::Warp,
            &default_warp_shader(0.97, wrap),
            &Options::default(),
        )
        .unwrap();
        assert_eq!(t.interface.textures.len(), 1);
        assert_eq!(
            t.interface.textures[0].address,
            if wrap { Address::Wrap } else { Address::Clamp }
        );
    }
}

#[test]
fn interface_samplers_texsize_and_blur_level() {
    let t = trg(
        PassKind::Warp,
        "sampler sampler_pw_noise_lq; sampler2D sampler_rand01_smalltiled; float4 texsize_rand01; float4 texsize_clouds; sampler sampler_fw_clouds;",
        "ret = tex2D(sampler_pw_noise_lq, uv*texsize_rand01.zw).xyz + tex2D(sampler_rand01_smalltiled, uv).xyz + GetBlur3(uv) + tex2D(sampler_fw_clouds, uv*texsize_clouds.zw).xyz;",
    );
    let names: Vec<&str> = t
        .interface
        .textures
        .iter()
        .map(|b| b.hlsl_name.as_str())
        .collect();
    // blur3 is an include.fx sampler (declared first), user samplers follow in declaration order
    assert_eq!(
        names,
        [
            "sampler_blur3",
            "sampler_pw_noise_lq",
            "sampler_rand01_smalltiled",
            "sampler_fw_clouds"
        ]
    );
    assert_eq!(t.interface.max_blur_level, 3);
    assert_eq!(t.texsize_params, ["texsize_rand01", "texsize_clouds"]);
    assert_eq!(
        t.interface.texsize_slots,
        [
            TexSource::User("rand01_smalltiled".into()),
            TexSource::User("clouds".into())
        ]
    );
    // WGSL bindings follow the ABI
    for i in 0..4u32 {
        assert!(t.wgsl.contains(&format!(
            "@group(1) @binding({}) var tex_",
            texture_binding(i)
        )));
        assert!(t.wgsl.contains(&format!(
            "@group(1) @binding({}) var smp_",
            sampler_binding(i)
        )));
    }
    assert!(t.wgsl.contains("pu.texsize[0]") && t.wgsl.contains("pu.texsize[1]"));
}

#[test]
fn unused_samplers_are_not_bound() {
    let t = trg(
        PassKind::Warp,
        "sampler sampler_pw_noise_lq; float4 texsize_foo;",
        "ret = tex2D(sampler_main, uv).xyz;",
    );
    assert_eq!(t.interface.textures.len(), 1);
    assert!(t.interface.texsize_slots.is_empty());
}

/// projectM declares every `sampler_<name>` / `texsize_<name>` a shader mentions (`GetReferencedSamplers`); MD2 would
/// reject the undeclared identifiers. Regression: M0Rf30 - Nebula Forge (`tex2D(sampler_pw_noise_lq, ..)`, no declaration).
#[test]
fn undeclared_samplers_are_declared_like_projectm() {
    let t = trg(
        PassKind::Comp,
        "",
        "float4 rn = tex2D(sampler_pw_noise_lq, (uv + 0.5) / 256.0); float v = tex3D(sampler_noisevol_hq, float3(uv, 0.5)).x;
         ret = rn.xyz * v + tex2D(sampler_fw_clouds, uv * texsize_clouds.zw).xyz; // sampler_fw_commented
        ",
    );
    let names: Vec<&str> = t
        .interface
        .textures
        .iter()
        .map(|b| b.hlsl_name.as_str())
        .collect();
    assert!(names.contains(&"sampler_pw_noise_lq") && names.contains(&"sampler_noisevol_hq"));
    assert!(names.contains(&"sampler_fw_clouds") && !names.contains(&"sampler_fw_commented"));
    let n = t
        .interface
        .textures
        .iter()
        .find(|b| b.hlsl_name == "sampler_pw_noise_lq")
        .unwrap();
    assert_eq!(
        (n.source.clone(), n.filter, n.address),
        (TexSource::Noise(NoiseTex::Lq), Filter::Point, Address::Wrap)
    );
    assert_eq!(t.texsize_params, ["texsize_clouds"]);
}

#[test]
fn include_fx_register_mapping() {
    let t = tr(
        PassKind::Warp,
        "float t = time + q5 + q32 + bass_att + mip_avg + blur2_min; float4 r = rand_frame + rand_preset + roam_cos + aspect + texsize + texsize_noise_lq + texsize_noisevol_hq;
         float3 p = mul(float4(uv,0,1), rot_s1) + mul(float4(uv,0,1), rot_uf4) + mul(float4(uv,0,1), rot_rand3);
         ret = t * r.xyz + p;",
    );
    let wgsl = t.wgsl.replace(['(', ')'], "");
    for needle in [
        "mu.c[4].x",
        "mu.q[1].x",
        "mu.q[7].w",
        "mu.c[4].x",
        "mu.c[12].z",
        "mu.c[13].x",
        "mu.rand_frame",
        "mu.rand_preset",
        "mu.c[8]",
        "mu.c[0]",
        "mu.c[7]",
        "mu.texsize_noise[0]",
        "mu.texsize_noise[5]",
        "mu.rot[0]",
        "mu.rot[19]",
        "mu.rot[22]",
    ] {
        assert!(wgsl.contains(needle), "missing {needle}:\n{}", t.wgsl);
    }
}

#[test]
fn comp_uses_uv_for_uv_orig_and_hue_shader() {
    let t = tr(
        PassKind::Comp,
        "ret = hue_shader * tex2D(sampler_main, uv_orig).xyz;",
    );
    assert!(t.wgsl.contains("in.uv.xy"));
    let w = tr(PassKind::Warp, "ret = tex2D(sampler_main, uv_orig).xyz;");
    assert!(w.wgsl.contains(".zw"));
    // hue_shader does not exist in warp shaders
    assert!(
        translate(
            PassKind::Warp,
            "shader_body { ret = hue_shader; }",
            &Options::default()
        )
        .is_err()
    );
}

#[test]
fn errors_are_reported_not_panicked() {
    let o = Options::default();
    assert!(matches!(
        translate(PassKind::Warp, "shader_body { ret = foo; }", &o),
        Err(TranslateError::Type(_))
    ));
    assert!(matches!(
        translate(PassKind::Warp, "shader_body { ret = ; }", &o),
        Err(TranslateError::Parse(_))
    ));
    assert!(matches!(
        translate(PassKind::Warp, "shader_body { ret = frobnicate(uv); }", &o),
        Err(TranslateError::Unsupported(_))
    ));
    assert!(matches!(
        translate(PassKind::Warp, "#if 1\nshader_body { ret = 1; }", &o),
        Err(TranslateError::Preprocess(_))
    ));
    assert!(matches!(
        translate(PassKind::Warp, "no body here", &o),
        Err(TranslateError::Wrap(_))
    ));
    // `float3 = float2` would need to invent a component
    assert!(
        translate(
            PassKind::Warp,
            "shader_body { float3 a = uv; ret = a; }",
            &o
        )
        .is_err()
    );
}

#[test]
fn preprocessor_subset() {
    let t = trg(
        PassKind::Warp,
        "#define K 3\n#define SQ(x) ((x)*(x))\n#define MyGet GetPixel\n#define sampler_pic sampler_cells\nsampler sampler_cells;\n#if 0\n ERROR_NOT_SEEN\n#else\n#ifdef K\n#undef K\n#define K 5\n#endif\n#endif",
        "#if K == 5 && defined(SQ)\nret = MyGet(uv) * SQ(K) + tex2D(sampler_pic, uv).xyz;\n#endif",
    );
    assert!(t.wgsl.contains("25.0f") || t.wgsl.contains("5.0f"));
    assert!(
        t.interface
            .textures
            .iter()
            .any(|b| b.hlsl_name == "sampler_cells" && b.source == TexSource::User("cells".into()))
    );
}

#[test]
fn translation_is_deterministic() {
    let src = "float2 f(float2 a){ return a*2; }\nshader_body { ret = tex2D(sampler_main, f(uv)).xyz * sin(time); }";
    let a = translate(PassKind::Warp, src, &Options::default()).unwrap();
    let b = translate(PassKind::Warp, src, &Options::default()).unwrap();
    assert_eq!(a.wgsl, b.wgsl);
    assert_eq!(a.interface, b.interface);
}

#[test]
fn derivative_uniformity_directive_is_optional() {
    let src = "shader_body { ret = tex2D(sampler_main, uv).xyz; }";
    let on = translate(PassKind::Warp, src, &Options::default()).unwrap();
    assert!(
        on.wgsl
            .starts_with("diagnostic(off, derivative_uniformity);")
    );
    let mut no_diag = Options::default();
    no_diag.derivative_uniformity_off = false;
    let off = translate(PassKind::Warp, src, &no_diag).unwrap();
    assert!(!off.wgsl.contains("diagnostic"));
}

/// Mutated corpus shaders (truncated / with deleted characters) must produce errors, never panics.
#[test]
fn malformed_input_never_panics() {
    let cases = corpus_cases();
    if cases.is_empty() {
        return;
    }
    let mut rng = common::gpu::Rng(0xfeed);
    let mut inputs: Vec<(PassKind, String)> = Vec::new();
    for c in cases.iter().step_by(7) {
        let chars: Vec<char> = c.text.chars().collect();
        if chars.len() < 4 {
            continue;
        }
        for _ in 0..3 {
            let cut = (rng.next() as usize) % chars.len();
            inputs.push((c.kind, chars[..cut].iter().collect()));
            let mut m = chars.clone();
            for _ in 0..3 {
                let k = (rng.next() as usize) % m.len();
                m.remove(k);
            }
            inputs.push((c.kind, m.iter().collect()));
            let mut m = chars.clone();
            let k = (rng.next() as usize) % m.len();
            m[k] = ['(', ')', '{', '}', ';', '.', ',', '[', '*', '#'][(rng.next() % 10) as usize];
            inputs.push((c.kind, m.iter().collect()));
        }
    }
    let panics = par_map(&inputs, |(k, s)| {
        let r = std::panic::catch_unwind(|| {
            let _ = translate(*k, s, &Options::default());
        });
        r.is_err().then(|| s.chars().take(200).collect::<String>())
    });
    let bad: Vec<_> = panics.into_iter().flatten().collect();
    assert!(
        bad.is_empty(),
        "{} panics, first input:\n{}",
        bad.len(),
        bad[0]
    );
}

/// projectM's hlslparser lowers float `%` and `fmod` to GLSL `mod` (`x - y*floor(x/y)`); HLSL truncates.
/// `Options::floor_mod` selects the former; integer `%` keeps its truncated semantics either way.
#[test]
fn floor_mod_option_selects_glsl_mod_for_floats() {
    let src = "shader_body { float a = uv.x * 8 - 4; ret.x = (a % 2.0) * 0.25; ret.y = fmod(a, 3.0) * 0.25; int i = (int)a; ret.z = (i % 2) * 0.25; }";
    let hlsl = translate(PassKind::Warp, src, &Options::default()).unwrap();
    let mut gl = Options::default();
    gl.floor_mod = true;
    let glsl = translate(PassKind::Warp, src, &gl).unwrap();
    naga_validate(&hlsl.wgsl).unwrap();
    naga_validate(&glsl.wgsl).unwrap();
    assert!(
        !hlsl.wgsl.contains("floor("),
        "HLSL semantics must not use floor"
    );
    assert_eq!(
        glsl.wgsl.matches("floor(").count(),
        2,
        "the float `%` and `fmod` use floor, the int `%` does not:\n{}",
        glsl.wgsl
    );
}

fn flavored(body: &str) -> String {
    let src = format!("shader_body {{ {body} }}");
    let mut opts = Options::default();
    opts.projectm_flavor = true;
    let t = translate(PassKind::Comp, &src, &opts).unwrap_or_else(|e| panic!("{e}\n{body}"));
    naga_validate(&t.wgsl).unwrap_or_else(|e| panic!("{e}\n{}", t.wgsl));
    t.wgsl
}

fn plain(body: &str) -> String {
    tr(PassKind::Comp, body).wgsl
}

/// Regression (`Flexi + bdrv - what to do [Phat edit]`, comp final 26 -> 99 dB): projectM's hlslparser resets the
/// operator priority after `(float2(..) ..)` and keeps swallowing every following operator, so
/// `a * (float2(..) + 1) - c` is `a * ((float2(..) + 1) - c)`.
#[test]
fn hlslparser_paren_constructor_priority_reset() {
    let body = "float2 a = uv - 0.5; float2 r = a / (float2(0.1, 0.2) + 0.5) - float2(0.0, 0.05); ret = float3(r, 0);";
    let pm = flavored(body);
    let std = plain(body);
    assert!(
        pm.contains("+ 0.5f) - vec2<f32>(0.0f, 0.05f)))"),
        "the subtraction must end up inside the multiplication's right operand:\n{pm}"
    );
    assert!(
        std.contains("+ 0.5f)) - vec2<f32>(0.0f, 0.05f))"),
        "textbook precedence subtracts after the product:\n{std}"
    );
}

/// Regression (`martin - organic light`, comp final 36 -> 99 dB): a variable that is read before it is written is
/// undefined, and Mesa's `nir_opt_undef` turns an undefined value used only by float arithmetic into NaN
/// (`max(x, NaN)` is `x`). `uv3 = .4 * cos(42 * uv3)`: the read is a `cos` operand -> NaN; a read that feeds a vector
/// constructor keeps the undefined value (default 0).
#[test]
fn undefined_variable_read_by_float_alu_is_nan_for_projectm() {
    let alu = "float2 u; u = .4 * cos(42 * u) + 0.1; ret = float3(u, 0);";
    let pm = flavored(alu);
    assert!(
        pm.contains("g_nan_bits"),
        "an ALU-only read of an undefined variable must start as NaN:\n{pm}"
    );
    let std = plain(alu);
    assert!(
        !std.contains("g_nan_bits"),
        "MilkDrop 2 semantics keep the zero default:\n{std}"
    );
    let ctor = "float2 u; float2 v = float2(u.x, 0.5); ret = float3(v, 0);";
    assert!(
        !flavored(ctor).contains("g_nan_bits"),
        "a vector-constructor use keeps the undef (not NaN)"
    );
    let defined = "float2 u = 0; u = .4 * cos(42 * u); ret = float3(u, 0);";
    assert!(!flavored(defined).contains("g_nan_bits"));
}

/// hlslparser: a cast parses `ParseExpression` after it, so `(int)x + 0.5` is `(int)(x + 0.5)`.
#[test]
fn hlslparser_cast_swallows_the_rest_of_the_expression() {
    let body = "float x = uv.x * 4; ret = (int)x + 0.5;";
    let pm = flavored(body);
    let std = plain(body);
    assert!(pm.contains("trunc((x_") && pm.contains(" + 0.5f)"), "{pm}");
    assert!(!std.contains("trunc((x_"), "{std}");
}

/// Regression (`The Waterfowl In The Rain`, `Starcraft (Distorted)`: NaN texture coordinates): asin / acos outside
/// [-1, 1] follow hlslparser's polynomial, not NaN.
#[test]
fn hlslparser_asin_acos_outside_unit_range() {
    let body = "ret = float3(asin(uv.x * 3), acos(uv.y * 3), 0);";
    let pm = flavored(body);
    assert!(pm.contains("0.39269908169872415f"), "{pm}");
    assert!(
        pm.contains("select(asin(") && pm.contains("select(acos("),
        "{pm}"
    );
    assert!(!plain(body).contains("select(asin("));
}

/// hlslparser prints float literals with 6 significant digits (`ostream << float`), the GLSL compiler parses that.
#[test]
fn hlslparser_float_literals_have_six_digits() {
    let body = "ret = uv.y - 0.4941668 + 0.00012345678 + 123456.789;";
    let pm = flavored(body);
    assert!(pm.contains("0.494167f"), "{pm}");
    assert!(pm.contains("0.000123457f"), "{pm}");
    assert!(pm.contains("123457.0f"), "{pm}");
    let std = plain(body);
    assert!(std.contains("0.4941668f"), "{std}");
}

/// Regression (`LuxXx - Done For the Night ii`, comp final 17 -> 98 dB): `v * x / float2(0, -1)` has a literal zero
/// divisor component; the GL oracle evaluates `0 / 0` to NaN there, so does the flavored translation.
#[test]
fn hlslparser_zero_literal_divisor_yields_nan() {
    let body = "float2 u = float2(0, -1) * texsize.zw / float2(0, -1); ret = float3(uv + u, 0);";
    let pm = flavored(body);
    assert!(pm.contains("bitcast<f32>"), "{pm}");
    assert!(!plain(body).contains("bitcast<f32>"));
}

#[test]
fn default_options_keep_textbook_hlsl_parsing() {
    // nothing of the flavor leaks into the default translation
    let t = plain("ret = (uv.x * (float2(1, 2) + 1) - 0.25).xxx;");
    assert!(t.contains("- 0.25f"), "{t}");
}

/// projectM compiles with `Flag_AlternateNanPropagation`: binary `*` is `mult()` (0 * inf = 0, no fused multiply-add),
/// `*=` and `mul()` stay plain products.
#[test]
fn hlslparser_binary_mul_is_mult() {
    let body = "float a = uv.x * uv.y; a *= 2; ret = a + mul(uv.x, 3);";
    let pm = flavored(body);
    assert_eq!(pm.matches("select((").count(), 1, "{pm}");
    assert!(!plain(body).contains("select(("));
}
