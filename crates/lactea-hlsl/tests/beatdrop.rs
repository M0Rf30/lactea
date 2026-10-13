//! BeatDrop shader inputs (`Options::beatdrop`): BeatDrop's `include.fx` additions (`_c14` / `_c15` / `_c16` and the
//! `mouse*` / `sysTime*` / `sysDate*` names, `sampler_fft`, `sampler_wave`, `HAS_FFT_PEAK`, `get_fft*`, `get_wave*`),
//! the spectrum / waveform lookup lowering and that nothing changes without the option.
mod common;

use common::*;
use lactea_abi::*;
use lactea_hlsl::*;

fn beatdrop() -> Options {
    let mut o = Options::default();
    o.beatdrop = true;
    o
}

fn src(globals: &str, body: &str) -> String {
    format!("{globals}\nshader_body\n{{\n{body}\n}}\n")
}

fn tr_with(globals: &str, body: &str, opts: &Options) -> Translated {
    let t = translate(PassKind::Comp, &src(globals, body), opts)
        .unwrap_or_else(|e| panic!("{e}\n{body}"));
    naga_validate(&t.wgsl).unwrap_or_else(|e| panic!("{e}\n{}", t.wgsl));
    t
}

fn bd(body: &str) -> Translated {
    tr_with("", body, &beatdrop())
}

/// `include.fx` of BeatDrop, the pieces this prelude adds to MilkDrop 2's, comments dropped (lines 18-20, 104,
/// 119-135, 142-147, 150-151, 154-155 and 187-231 of the file).
const PRELUDE: &str = "float4 _c14;
float4 _c15;
float4 _c16;
#define aspect_sd _c1
#define mouse _c14
#define mouse_x _c14.x
#define mouse_y _c14.y
#define mouse_pos _c14.xy
#define mouse_clicked _c14.z
#define mouse_released _c14.w
#define sysTime _c15
#define sysDate _c16
#define sysTimeHour _c15.x
#define sysTimeMinute _c15.y
#define sysTimeSecond _c15.z
#define sysTimeTotalSeconds _c15.w
#define sysDateYear _c16.x
#define sysDateMonth _c16.y
#define sysDateDay _c16.z
#define sysDateWeekday _c16.w
#define SAMPLER1D sampler1D
#define SAMPLER2D sampler2D
#define SAMPLER3D sampler3D
#define texture1D tex1D
#define texture2D tex2D
#define texture3D tex3D
#define trunc(x) ((x) < 0 ? ceil((x)) : floor((x)))
#define tex1d tex1D
#define texcube texCUBE
#define texCube texCUBE
sampler2D sampler_fft;
#define HAS_FFT_PEAK 1
float get_fft(float pos) {
    return sqrt(tex2D(sampler_fft, float2(saturate(pos), .25)).x);
}
float get_fft_hz(float freq) {
    return get_fft(freq / 22050.0);
}
float get_fft_peak(float pos) {
    return sqrt(tex2D(sampler_fft, float2(saturate(pos), 0.75)).x);
}
float get_fft_peak_hz(float freq) {
    return get_fft_peak(freq / 22050.0);
}
sampler2D sampler_wave;
float get_wave(float pos) {
    float left = tex2D(sampler_wave, float2(saturate(pos), 0.25)).x;
    float right = tex2D(sampler_wave, float2(saturate(pos), 0.75)).x;
    return (left + right) * 0.5;
}
float get_wave_left(float pos) {
    return tex2D(sampler_wave, float2(saturate(pos), 0.25)).x;
}
float get_wave_right(float pos) {
    return tex2D(sampler_wave, float2(saturate(pos), 0.75)).x;
}
";

#[test]
fn prelude_is_beatdrops_include_additions_between_include_and_pass_defines() {
    let user = "float4 mine;\nshader_body\n{\n ret = 1;\n}\n";
    let md2 = md2_wrapped_hlsl(PassKind::Warp, user).unwrap();
    let bd = beatdrop_wrapped_hlsl(PassKind::Warp, user).unwrap();
    let i_inc = bd.find("float4   rand_frame").unwrap();
    let i_c14 = bd.find("float4 _c14;").unwrap();
    let i_def = bd.find("#define uv_orig _uv.zw").unwrap();
    let i_usr = bd.find("float4 mine;").unwrap();
    assert!(i_inc < i_c14 && i_c14 < i_def && i_def < i_usr);
    assert!(!md2.contains("_c14") && !md2.contains("get_fft"));
    // the BeatDrop wrapper is the MD2 one plus exactly the prelude block, which is the file's text
    let end = bd.find("#define rad _rad_ang.x").unwrap();
    assert_eq!(&bd[i_c14..end], PRELUDE);
    assert_eq!(format!("{}{}", &bd[..i_c14], &bd[end..]), md2);
}

#[test]
fn a_shader_that_defines_its_own_get_fft_keeps_it() {
    let user =
        "float get_fft(float x) { return x * 2; }\nshader_body\n{\n ret = get_fft(0.25);\n}\n";
    let bd = beatdrop_wrapped_hlsl(PassKind::Comp, user).unwrap();
    assert!(
        !bd.contains("float get_fft(float pos)"),
        "user's function wins"
    );
    assert!(
        !bd.contains("float get_fft_hz("),
        "get_fft_hz calls get_fft: it would reach the user's, declared further down"
    );
    for kept in [
        "float get_fft_peak(float pos)",
        "float get_fft_peak_hz(",
        "float get_wave(float pos)",
        "float get_wave_left(",
        "float get_wave_right(",
        "#define HAS_FFT_PEAK 1",
    ] {
        assert!(bd.contains(kept), "{kept}");
    }
    let t = translate(PassKind::Comp, user, &beatdrop()).unwrap();
    naga_validate(&t.wgsl).unwrap();
    assert!(
        !t.wgsl.contains("bd_fetch("),
        "the user's function does not sample"
    );
}

#[test]
fn a_shader_that_declares_a_prelude_macro_name_keeps_its_own() {
    // a local named like one of the `mouse*` / `sysTime*` macros must not be rewritten into `_c14`
    let t = tr_with(
        "",
        "float2 mouse = uv * 2; float sysTime = mouse.x; ret = float3(mouse, sysTime);",
        &beatdrop(),
    );
    assert!(!t.wgsl.contains("mx.mouse"), "{}", t.wgsl);
    // ... while the other names stay macros
    let t = tr_with(
        "",
        "float2 mouse = uv * 2; ret = float3(mouse, mouse_x);",
        &beatdrop(),
    );
    assert!(t.wgsl.contains("mx.mouse"), "{}", t.wgsl);
}

#[test]
fn lookups_bind_two_separate_textures_and_use_both_axes() {
    let t = bd("ret = float3(get_fft(0.5), get_fft_peak(0.5), get_wave(0.25));");
    let by_name = |n: &str| {
        t.interface
            .textures
            .iter()
            .find(|b| b.hlsl_name == n)
            .unwrap_or_else(|| panic!("no {n}: {:?}", t.interface.textures))
    };
    let fft = by_name("sampler_fft");
    let wave = by_name("sampler_wave");
    assert_eq!(fft.source, TexSource::Fft, "1024 x 2, own binding");
    assert_eq!(
        wave.source,
        TexSource::User("wave".into()),
        "480 x 2, own binding"
    );
    for b in [fft, wave] {
        assert_eq!(
            (b.filter, b.address, b.dim),
            (Filter::Linear, Address::Clamp, TexDim::D2),
            "{}",
            b.hlsl_name
        );
    }
    // one helper reads both textures by their own size, MilkDrop 3's is not emitted
    assert!(t.wgsl.contains("fn bd_fetch("));
    assert!(t.wgsl.contains("textureDimensions(t)"));
    assert!(t.wgsl.contains("bd_fetch(tex_sampler_fft, "));
    assert!(t.wgsl.contains("bd_fetch(tex_sampler_wave, "));
    assert!(!t.wgsl.contains("fn fft_fetch("));
}

#[test]
fn the_functions_use_the_row_centres_and_the_prelude_arithmetic() {
    let t = bd("ret = float3(get_fft(0.5), get_fft_peak_hz(1000), get_fft_hz(20000));");
    // `.25` / `.75` select the row, `sqrt` is taken, the hertz variants divide by 22050
    assert!(t.wgsl.contains("0.25"), "{}", t.wgsl);
    assert!(t.wgsl.contains("0.75"), "{}", t.wgsl);
    assert!(t.wgsl.contains("sqrt("), "{}", t.wgsl);
    assert!(t.wgsl.contains("22050.0"), "{}", t.wgsl);
    // only the spectrum texture is bound
    assert_eq!(t.interface.textures.len(), 1);
    assert_eq!(t.interface.textures[0].source, TexSource::Fft);
}

#[test]
fn get_wave_is_the_mono_average_of_both_rows_and_the_channels_stay_available() {
    let t = bd("ret = get_wave(0.5).xxx;");
    assert_eq!(t.interface.textures.len(), 1);
    assert_eq!(
        t.interface.textures[0].source,
        TexSource::User("wave".into())
    );
    // both rows of the 480 x 2 texture are fetched, then averaged
    assert_eq!(
        t.wgsl.matches("bd_fetch(tex_sampler_wave, ").count(),
        2,
        "{}",
        t.wgsl
    );
    assert!(t.wgsl.contains("0.25") && t.wgsl.contains("0.75"));
    assert!(t.wgsl.contains("0.5"), "{}", t.wgsl);

    // the left / right helpers read one row each
    let l = bd("ret = get_wave_left(0.5).xxx;");
    assert_eq!(l.wgsl.matches("bd_fetch(tex_sampler_wave, ").count(), 1);
    assert!(l.wgsl.contains("0.25") && !l.wgsl.contains("0.75"));
    let r = bd("ret = get_wave_right(0.5).xxx;");
    assert_eq!(r.wgsl.matches("bd_fetch(tex_sampler_wave, ").count(), 1);
    assert!(r.wgsl.contains("0.75") && !r.wgsl.contains("0.25"));
}

#[test]
fn direct_lookups_of_the_samplers_work_and_follow_the_sampler_modes() {
    // `tex2D(sampler_fft, ..)` by the user: bilinear + clamp by default, both coordinates used
    let t = bd("ret = tex2D(sampler_fft, float2(uv.x, 0.75)).xxx + tex2D(sampler_wave, uv).yzx;");
    assert!(t.wgsl.contains("bd_fetch(tex_sampler_fft, "), "{}", t.wgsl);
    assert!(
        t.wgsl.contains("true, false)"),
        "linear, clamped: {}",
        t.wgsl
    );
    // the prefixes pick point / wrap like for any sampler
    let t = tr_with(
        "sampler2D sampler_pw_fft; sampler2D sampler_fw_wave;",
        "ret = tex2D(sampler_pw_fft, uv).xxx + tex2D(sampler_fw_wave, uv).xxx;",
        &beatdrop(),
    );
    assert!(
        t.wgsl.contains("false, true)"),
        "point, wrapped: {}",
        t.wgsl
    );
    assert!(
        t.wgsl.contains("true, true)"),
        "linear, wrapped: {}",
        t.wgsl
    );
    // lod / bias / proj / grad flavours are lowered as well
    bd(
        "ret = tex2Dlod(sampler_fft, float4(uv, 0, 0)).xxx + tex2Dbias(sampler_wave, float4(uv, 0, 1)).xxx + tex2Dproj(sampler_fft, float4(uv, 0, 1)).xxx;",
    );
}

#[test]
fn texsize_fft_and_texsize_wave_name_the_two_textures() {
    let t = tr_with(
        "float4 texsize_fft; float4 texsize_wave;",
        "ret = float3(texsize_fft.x / 1024.0, texsize_wave.x / 480.0, texsize_wave.w) + tex2D(sampler_fft, uv).xxx;",
        &beatdrop(),
    );
    assert_eq!(t.texsize_params, ["texsize_fft", "texsize_wave"]);
    assert_eq!(
        t.interface.texsize_slots,
        [TexSource::Fft, TexSource::User("wave".into())]
    );
}

#[test]
fn has_fft_peak_and_the_mouse_and_clock_macros() {
    let t = bd(
        "#ifdef HAS_FFT_PEAK\nret = get_fft_peak(0.1).xxx;\n#else\nret = 0;\n#endif\nret += float3(mouse_x, mouse_y, mouse_clicked) + float3(mouse_pos, mouse_released) + mouse.xyz;",
    );
    assert!(t.wgsl.contains("bd_fetch("), "HAS_FFT_PEAK is defined");
    assert!(t.wgsl.contains("mx.mouse"), "{}", t.wgsl);
    // sysTime / sysDate are `_c15` / `_c16`: the first two q_hi registers of the extension block
    let t = bd(
        "ret = float3(sysTimeHour, sysTimeMinute, sysTimeSecond) + sysTimeTotalSeconds + sysTime.xyz \
         + float3(sysDateYear, sysDateMonth, sysDateDay) + sysDateWeekday + sysDate.xyz;",
    );
    assert!(t.wgsl.contains("mx.q_hi[0]"), "{}", t.wgsl);
    assert!(t.wgsl.contains("mx.q_hi[1]"), "{}", t.wgsl);
    assert!(!t.wgsl.contains("mx.mouse"));
}

#[test]
fn time_and_date_constants_map_onto_the_extension_block() {
    let t = bd("ret = float3(_c14.x, _c15.w / 86400.0, _c16.y / 12.0);");
    assert!(t.wgsl.contains("mx.mouse"), "{}", t.wgsl);
    assert!(t.wgsl.contains("mx.q_hi[0]"));
    assert!(t.wgsl.contains("mx.q_hi[1]"));
}

#[test]
fn without_the_option_the_names_do_not_exist() {
    for body in [
        "ret = float3(get_fft(0.5), 0, 0);",
        "ret = get_wave(0.5).xxx;",
        "ret = get_wave_left(0.5).xxx;",
        "ret = mouse_x.xxx;",
        "ret = sysTimeHour.xxx;",
        "ret = _c15.xyz;",
    ] {
        assert!(
            translate(PassKind::Comp, &src("", body), &Options::default()).is_err(),
            "{body}"
        );
    }
    // `sampler_fft` and `sampler_wave` stay user textures (what projectM and plain MD2 translate)
    let t = tr_with(
        "",
        "ret = tex2D(sampler_fft, uv).xyz + tex2D(sampler_wave, uv).xyz;",
        &Options::default(),
    );
    assert_eq!(
        t.interface.textures[0].source,
        TexSource::User("fft".into())
    );
    assert_eq!(
        t.interface.textures[1].source,
        TexSource::User("wave".into())
    );
    assert!(!t.wgsl.contains("bd_fetch"));
}

#[test]
fn aspect_sd_is_the_screen_dependent_aspect_constant() {
    let t = bd("ret = float3(aspect_sd.x, aspect_sd.y, aspect.y);");
    assert!(t.wgsl.contains("mu.c[1]"), "{}", t.wgsl);
    assert!(t.wgsl.contains("mu.c[0]"), "{}", t.wgsl);
    assert!(
        translate(
            PassKind::Comp,
            &src("", "ret = aspect_sd.xyz;"),
            &Options::default()
        )
        .is_err(),
        "MilkDrop 2 proper has no `aspect_sd`"
    );
}

#[test]
fn the_sampler_and_texture_aliases_expand_like_the_file_says() {
    // `SAMPLER2D` / `texture2D` are `sampler2D` / `tex2D`; the declared sampler is not declared a second time
    let t = tr_with(
        "SAMPLER2D sampler_extra;",
        "ret = texture2D(sampler_extra, uv).xyz + tex2d(sampler_extra, uv).xyz;",
        &beatdrop(),
    );
    assert_eq!(t.interface.textures.len(), 1);
    assert_eq!(
        (
            t.interface.textures[0].source.clone(),
            t.interface.textures[0].dim
        ),
        (TexSource::User("extra".into()), TexDim::D2)
    );
    // `SAMPLER3D` / `texture3D` give a volume binding
    let t = tr_with(
        "SAMPLER3D sampler_vol;",
        "ret = texture3D(sampler_vol, float3(uv, 0.5)).xyz;",
        &beatdrop(),
    );
    assert_eq!(t.interface.textures.len(), 1);
    assert_eq!(t.interface.textures[0].dim, TexDim::D3);
    // the 1D and cube spellings expand to `tex1D` / `texCUBE` (which the translator has no lowering for)
    for body in [
        "ret = texture1D(sampler_main, uv.x).xyz;",
        "ret = tex1d(sampler_main, uv.x).xyz;",
        "ret = texcube(sampler_main, float3(uv, 0)).xyz;",
        "ret = texCube(sampler_main, float3(uv, 0)).xyz;",
    ] {
        let e = translate(PassKind::Comp, &src("", body), &beatdrop()).unwrap_err();
        let m = e.message();
        assert!(m.contains("tex1D") || m.contains("texCUBE"), "{body}: {m}");
    }
    // without the option none of them is a name
    for body in [
        "ret = texture3D(sampler_main, float3(uv, 0.5)).xyz;",
        "ret = tex1d(sampler_main, uv.x).xyz;",
    ] {
        assert!(
            translate(PassKind::Comp, &src("", body), &Options::default()).is_err(),
            "{body}"
        );
    }
    assert!(
        translate(
            PassKind::Comp,
            &src(
                "SAMPLER2D sampler_extra;",
                "ret = tex2D(sampler_extra, uv).xyz;"
            ),
            &Options::default()
        )
        .is_err(),
        "`SAMPLER2D` is not a type without the prelude"
    );
}

#[test]
fn trunc_is_the_files_ceil_floor_macro() {
    let body = "ret = trunc(uv.xyx * 4 - 2) * 0.25 + 0.5;";
    let t = bd(body);
    // `((x) < 0 ? ceil((x)) : floor((x)))`: a component-wise select between the two rounding functions
    assert!(t.wgsl.contains("ceil("), "{}", t.wgsl);
    assert!(t.wgsl.contains("floor("), "{}", t.wgsl);
    assert!(t.wgsl.contains("select("), "{}", t.wgsl);
    assert!(!t.wgsl.contains("trunc("), "{}", t.wgsl);
    // the intrinsic itself stays what MilkDrop 2 proper (and projectM) translate
    let t = tr_with("", body, &Options::default());
    assert!(t.wgsl.contains("trunc("), "{}", t.wgsl);
    assert!(!t.wgsl.contains("ceil("), "{}", t.wgsl);
    // a scalar works too
    bd("ret = trunc(uv.x * 4 - 2).xxx;");
}

#[test]
fn a_shader_that_declares_one_of_the_extra_names_keeps_its_own() {
    let user = "float3 trunc(float3 v) { return v; }\nfloat4 aspect_sd;\nshader_body\n{\n ret = trunc(float3(uv, 0.5));\n}\n";
    let bd = beatdrop_wrapped_hlsl(PassKind::Comp, user).unwrap();
    assert!(!bd.contains("#define trunc("), "the user's function wins");
    assert!(
        !bd.contains("#define aspect_sd "),
        "the user's constant wins"
    );
    for kept in [
        "#define texture2D tex2D",
        "#define SAMPLER2D sampler2D",
        "#define tex1d tex1D",
        "#define texCube texCUBE",
    ] {
        assert!(bd.contains(kept), "{kept}");
    }
    let t = tr_with(
        "float3 trunc(float3 v) { return v * 0.5; }",
        "ret = trunc(float3(uv, 0.5));",
        &beatdrop(),
    );
    assert!(!t.wgsl.contains("select("), "{}", t.wgsl);
}
