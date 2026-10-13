//! BeatDrop shader inputs (`Options::beatdrop`): the prelude (`_c14` / `_c15` / `_c16`, `sampler_fft`, `sampler_wave`,
//! `get_fft*`, `get_wave`), the spectrum / waveform lookup lowering and that nothing changes without the option.
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

#[test]
fn prelude_sits_between_include_and_pass_defines_and_leaves_md2_alone() {
    let user = "float4 mine;\nshader_body\n{\n ret = 1;\n}\n";
    let md2 = md2_wrapped_hlsl(PassKind::Warp, user).unwrap();
    let bd = beatdrop_wrapped_hlsl(PassKind::Warp, user).unwrap();
    let i_inc = bd.find("float4   rand_frame").unwrap();
    let i_c14 = bd.find("float4 _c14, _c15, _c16;").unwrap();
    let i_def = bd.find("#define uv_orig _uv.zw").unwrap();
    let i_usr = bd.find("float4 mine;").unwrap();
    assert!(i_inc < i_c14 && i_c14 < i_def && i_def < i_usr);
    assert!(!md2.contains("_c14") && !md2.contains("get_fft"));
    // the BeatDrop wrapper is the MD2 one plus exactly the prelude block
    let end = bd.find("#define rad _rad_ang.x").unwrap();
    assert_eq!(format!("{}{}", &bd[..i_c14], &bd[end..]), md2);
    for m in ["get_fft(", "get_fft_hz(", "get_fft_peak(", "get_wave("] {
        assert!(bd.contains(&format!("#define {m}")), "{m}");
    }
}

#[test]
fn a_shader_that_defines_its_own_get_fft_keeps_it() {
    let user =
        "float get_fft(float x) { return x * 2; }\nshader_body\n{\n ret = get_fft(0.25);\n}\n";
    let bd = beatdrop_wrapped_hlsl(PassKind::Comp, user).unwrap();
    assert!(!bd.contains("#define get_fft("), "user's function wins");
    assert!(bd.contains("#define get_fft_hz("), "the other helpers stay");
    let t = translate(PassKind::Comp, user, &beatdrop()).unwrap();
    naga_validate(&t.wgsl).unwrap();
    assert!(
        !t.wgsl.contains("bd_fetch("),
        "the user's function does not sample"
    );
}

#[test]
fn lookups_bind_one_engine_texture_and_use_both_axes() {
    let t = tr_with(
        "",
        "ret = float3(get_fft(0.5), get_fft_peak(0.5), get_wave(0.25));",
        &beatdrop(),
    );
    let fft: Vec<_> = t
        .interface
        .textures
        .iter()
        .filter(|b| b.source == TexSource::Fft)
        .collect();
    assert_eq!(fft.len(), 2, "sampler_fft and sampler_wave");
    assert!(
        fft.iter().all(|b| b.filter == Filter::Linear
            && b.address == Address::Clamp
            && b.dim == TexDim::D2)
    );
    // spectrum rows start at 0, waveform rows at 2 (see `BEATDROP_ROWS`)
    assert!(t.wgsl.contains("fn bd_fetch("));
    assert!(
        t.wgsl
            .contains(&format!("{}, 0, true, false)", BEATDROP_FFT_BINS))
    );
    assert!(
        t.wgsl
            .contains(&format!("{}, 2, true, false)", BEATDROP_WAVE_SAMPLES))
    );
    // MilkDrop 3's helper is not emitted
    assert!(!t.wgsl.contains("fn fft_fetch("));
}

#[test]
fn time_and_date_constants_map_onto_the_extension_block() {
    let t = tr_with(
        "",
        "ret = float3(_c14.x, _c15.w / 86400.0, _c16.y / 12.0);",
        &beatdrop(),
    );
    assert!(t.wgsl.contains("mx.mouse"), "{}", t.wgsl);
    assert!(t.wgsl.contains("mx.q_hi[0]"));
    assert!(t.wgsl.contains("mx.q_hi[1]"));
}

#[test]
fn without_the_option_the_names_do_not_exist() {
    let body = "ret = float3(get_fft(0.5), 0, 0);";
    assert!(translate(PassKind::Comp, &src("", body), &Options::default()).is_err());
    let body = "ret = _c15.xyz;";
    assert!(translate(PassKind::Comp, &src("", body), &Options::default()).is_err());
    // `sampler_fft` stays a user texture named `fft` (what projectM and plain MD2 translate)
    let t = tr_with("", "ret = tex2D(sampler_fft, uv).xyz;", &Options::default());
    assert_eq!(
        t.interface.textures[0].source,
        TexSource::User("fft".into())
    );
}
