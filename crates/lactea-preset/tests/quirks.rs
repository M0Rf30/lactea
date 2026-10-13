use lactea_preset::*;

fn p(s: &str) -> Preset {
    Preset::parse(s.as_bytes()).unwrap()
}

#[test]
fn defaults_on_garbage() {
    let d = p("hello world\n");
    assert_eq!(d.milkdrop_preset_version, 100);
    assert_eq!(d.warp_ps_version, 0);
    assert_eq!(d.base.decay, 0.98);
    assert_eq!(d.base.gamma_adj, 2.0);
    assert_eq!(d.base.mv_x, 12.0);
    assert!(d.base.tex_wrap);
    assert!(d.base.maximize_wave_color);
    assert_eq!(d.waves.len(), 16);
    assert_eq!(d.shapes.len(), 16);
    assert_eq!(d.waves[3].samples, 512);
    assert_eq!(d.shapes[3].sides, 4);
    assert_eq!(d.shapes[3].border_a, 0.1);
    assert_eq!(d.base.ib_r, 0.25);
    assert_eq!(d.base.blur1_edge_darken, 0.25);
    assert_eq!(Preset::parse(b""), Err(ParseError::Empty));
}

#[test]
fn quirk_wave_color_fallback_is_rot() {
    let d = p("[preset00]\nfRating=1\n");
    assert_eq!(
        (d.base.wave_r, d.base.wave_g, d.base.wave_b),
        (0.0, 0.0, 0.0)
    );
    assert_eq!((d.base.wave_x, d.base.wave_y), (0.0, 0.0));
    // CState::Default() itself keeps 1/1/1/.5/.5
    assert_eq!(BaseValues::default().wave_r, 1.0);
    assert_eq!(BaseValues::default().wave_x, 0.5);
}

#[test]
fn quirk_mv_a_from_legacy_flag() {
    assert_eq!(p("a=1\n").base.mv_a, 0.0);
    assert_eq!(p("bMotionVectorsOn=1\n").base.mv_a, 1.0);
    assert_eq!(p("bMotionVectorsOn=1\nmv_a=0.25\n").base.mv_a, 0.25);
    assert_eq!(p("bMotionVectorsOn=0\nmv_a=0.5\n").base.mv_a, 0.5);
}

#[test]
fn versions() {
    let d = p("MILKDROP_PRESET_VERSION=200\nPSVERSION=3\nPSVERSION_WARP=4\n");
    assert_eq!((d.warp_ps_version, d.comp_ps_version), (3, 3));
    let d = p("MILKDROP_PRESET_VERSION=200\n");
    assert_eq!((d.warp_ps_version, d.comp_ps_version), (2, 2));
    let d = p("MILKDROP_PRESET_VERSION=201\nPSVERSION=3\nPSVERSION_COMP=4\n");
    assert_eq!((d.warp_ps_version, d.comp_ps_version), (2, 4));
    assert_eq!(d.max_ps_version(), 4);
    assert_eq!(d.min_ps_version(), 2);
    let d = p("MILKDROP_PRESET_VERSION=199\nPSVERSION=3\nPSVERSION_WARP=4\n");
    assert_eq!((d.warp_ps_version, d.comp_ps_version), (0, 0));
    // no version key at all -> 100 -> no shaders even if text is present
    let d = p("warp_1=`x\n");
    assert_eq!(d.milkdrop_preset_version, 100);
    assert_eq!(d.warp_shader.as_deref(), Some("x\u{1}"));
    assert_eq!(d.warp_ps_version, 0);
}

#[test]
fn case_sensitive_keys() {
    let d = p("fdecay=0.5\nFDECAY=0.4\n");
    assert_eq!(d.base.decay, 0.98);
    assert!(d.extra.iter().any(|(k, _)| k == "fdecay"));
}

#[test]
fn sections_ignored_whole_file_search() {
    let d = p("[preset00]\nfDecay=0.5\n[other]\nzoom=1.5\n");
    assert_eq!(d.base.decay, 0.5);
    assert_eq!(d.base.zoom, 1.5);
}

#[test]
fn duplicates_first_wins_by_search() {
    let d = p("zoom=1.5\nrot=0.1\nzoom=2.5\n");
    assert_eq!(d.base.zoom, 1.5);
}

#[test]
fn duplicates_cursor_quirk() {
    // Lookup order in MD2: ... "fDecay" then (many keys) ... The cursor makes a key whose
    // *next-line* duplicate directly follows the previous hit win over an earlier copy.
    // fVideoEchoZoom is looked up right after fDecay,... (fDecay, FFTAttack, FFTDecay,
    // fGammaAdj, fVideoEchoZoom): put an early copy of fVideoEchoZoom first and a copy
    // right after fGammaAdj.
    let d = p("fVideoEchoZoom=3\nfGammaAdj=1\nfVideoEchoZoom=4\n");
    assert_eq!(d.base.gamma_adj, 1.0);
    assert_eq!(
        d.base.video_echo_zoom, 4.0,
        "cursor hit beats the earlier duplicate"
    );
    let d = p("fVideoEchoZoom=3\nfGammaAdj=1\nfDecay=.5\nfVideoEchoZoom=4\n");
    assert_eq!(d.base.video_echo_zoom, 3.0);
}

#[test]
fn space_separator_and_equals_in_value() {
    let d = p("fDecay 0.5\nzoom = 1.5\nrot=  0.25\n");
    assert_eq!(d.base.decay, 0.5);
    assert_eq!(
        d.base.zoom, 1.0,
        "'zoom = 1.5': value is '= 1.5' -> sscanf fails -> default"
    );
    assert_eq!(d.base.rot, 0.25);
}

#[test]
fn line_endings_and_bom() {
    let d =
        Preset::parse(b"\xEF\xBB\xBF[preset00]\r\nfDecay=0.5\r\n\r\n\r\nzoom=1.25\r\n").unwrap();
    assert_eq!(d.base.decay, 0.5);
    assert_eq!(d.base.zoom, 1.25);
    // BOM glued to the first key (no [preset00] header): that key is lost
    let d = Preset::parse(b"\xEF\xBB\xBFfDecay=0.5\nzoom=1.25\n").unwrap();
    assert_eq!(d.base.decay, 0.98);
    assert_eq!(d.base.zoom, 1.25);
    // bare CR line endings: MD2's fgets runs to EOF -> only the first line is an entry
    let d = Preset::parse(b"fDecay=0.5\rzoom=1.25\r").unwrap();
    assert_eq!(d.base.decay, 0.5);
    assert_eq!(d.base.zoom, 1.0);
    assert_eq!(Preset::parse(b"\xFF\xFEa"), Err(ParseError::Utf16));
}

#[test]
fn leading_whitespace_breaks_key() {
    assert_eq!(p(" fDecay=0.5\n").base.decay, 0.98);
    // trailing whitespace on value is harmless; trailing on key is part of the sep
    assert_eq!(p("fDecay=0.5   \n").base.decay, 0.5);
}

#[test]
fn numeric_garbage() {
    let d = p("fDecay=abc\nfGammaAdj=1.5xyz\nnWaveMode=3.9\nzoom=1e\nrot=.5\ncx=\ncy=-\n");
    assert_eq!(d.base.decay, 0.98);
    assert_eq!(d.base.gamma_adj, 1.5);
    assert_eq!(d.base.wave_mode, 3);
    assert_eq!(d.base.zoom, 1.0); // "1e": glibc matching failure
    assert_eq!(d.base.rot, 0.5);
    assert_eq!(d.base.cx, 0.5);
    assert_eq!(d.base.cy, 0.5);
    let d = p("bWaveThick=2\nbTexWrap=0\nbDarken=-1\nbInvert=x\n");
    assert!(d.base.wave_thick && !d.base.tex_wrap && d.base.darken && !d.base.invert);
}

#[test]
fn non_utf8_is_latin1() {
    let d = Preset::parse(b"per_frame_1=x=1; // caf\xE9\n").unwrap();
    assert_eq!(d.per_frame, "x=1; // caf\u{e9}\u{1}");
    let d = Preset::parse("per_frame_1=x=1; // café\n".as_bytes()).unwrap();
    assert_eq!(d.per_frame, "x=1; // café\u{1}");
}

#[test]
fn byte_ff_is_eof() {
    // 0xFF at a line start terminates the scan (signed char == EOF)
    let d = Preset::parse(b"fDecay=0.5\n\xFFzoom=1.5\nrot=1\n").unwrap();
    assert_eq!(d.base.decay, 0.5);
    assert_eq!(d.base.zoom, 1.0);
    assert_eq!(d.base.rot, 0.0);
    // 0xFF inside a value truncates the value
    let d = Preset::parse(b"per_frame_1=a=1;\xFFb=2;\nper_frame_2=c=3;\n").unwrap();
    assert_eq!(d.per_frame, "a=1;\u{1}c=3;\u{1}");
}

#[test]
fn code_joining() {
    let d = p(
        "per_frame_1=a=1;\nper_frame_2=\nper_frame_3=b=2;\nper_frame_5=c=3;\nper_pixel_2=x=1;\nper_frame_init_1=q1=1;\n",
    );
    assert_eq!(d.per_frame, "a=1;\u{1}\u{1}b=2;\u{1}");
    assert_eq!(d.per_pixel, "", "numbering must start at 1");
    assert_eq!(d.per_frame_init, "q1=1;\u{1}");
    assert!(d.extra.iter().any(|(k, _)| k == "per_frame_5"));
    assert!(d.extra.iter().any(|(k, _)| k == "per_pixel_2"));
    assert_eq!(
        code_lines(&d.per_frame).collect::<Vec<_>>(),
        ["a=1;", "", "b=2;"]
    );
    assert_eq!(code_to_source("a;\u{1}b;\u{1}"), "a;\nb;\n");
}

#[test]
fn code_end_sentinel() {
    let d = p("per_frame_1=a=1;\nper_frame_2=~!@#$\nper_frame_3=b;\n");
    assert_eq!(d.per_frame, "a=1;\u{1}");
}

#[test]
fn backtick_stripping() {
    let d = p("warp_1=`shader_body {\nwarp_2=`  ret = 1;\nwarp_3=}\ncomp_1=`x\n");
    assert_eq!(
        d.warp_shader.as_deref(),
        Some("shader_body {\u{1}  ret = 1;\u{1}}\u{1}")
    );
    assert_eq!(d.comp_shader.as_deref(), Some("x\u{1}"));
    let d = p("fRating=3\n");
    assert_eq!(d.warp_shader, None);
    assert_eq!(d.comp_shader, None);
    // only the first backtick is stripped
    let d = p("per_frame_1=``a\n");
    assert_eq!(d.per_frame, "`a\u{1}");
}

#[test]
fn custom_wave_and_shape() {
    let d = p(
        "wavecode_0_enabled=1\nwavecode_0_samples=100\nwavecode_0_r=0.25\nwave_0_per_point1=x=sample;\nwave_0_per_point2=y=value1;\nwave_15_init1=q1=1;\nwavecode_16_enabled=1\nshapecode_2_enabled=1\nshapecode_2_sides=7\nshapecode_2_num_inst=5\nshape_2_per_frame1=rad=.2;\nshapecode_16_enabled=1\n",
    );
    assert_eq!(d.waves[0].enabled, 1);
    assert_eq!(d.waves[0].samples, 100);
    assert_eq!(d.waves[0].r, 0.25);
    assert_eq!(d.waves[0].g, 1.0);
    assert_eq!(d.waves[0].per_point, "x=sample;\u{1}y=value1;\u{1}");
    assert_eq!(d.waves[15].init, "q1=1;\u{1}");
    assert_eq!(d.shapes[2].sides, 7);
    assert_eq!(d.shapes[2].instances, 5);
    assert_eq!(d.shapes[2].per_frame, "rad=.2;\u{1}");
    assert!(d.extra.iter().any(|(k, _)| k == "wavecode_16_enabled"));
    assert!(d.extra.iter().any(|(k, _)| k == "shapecode_16_enabled"));
}

#[test]
fn long_line_creates_bogus_entries() {
    // Lines > 251 chars after the separator are cut by the scan's fgets; the tail is
    // scanned as new "lines".  A tail that looks like "zoom=3" becomes a real entry.
    let pad = "a".repeat(251);
    let text = format!("per_frame_1={pad}zoom=3\n");
    let d = p(&text);
    assert_eq!(d.base.zoom, 3.0);
    // but the code value itself is read in full by the late lookup
    assert_eq!(d.per_frame, format!("{pad}zoom=3\u{1}"));
}

#[test]
fn roundtrip_handbuilt() {
    let mut a = Preset::default();
    a.milkdrop_preset_version = 201;
    a.warp_ps_version = 2;
    a.comp_ps_version = 4;
    a.base.decay = 0.9;
    a.base.wave_r = 0.123456;
    a.per_frame = "a=1;\u{1}`b=2;\u{1}\u{1}c=3;\u{1}".into();
    a.warp_shader = Some("shader_body {\u{1}}\u{1}".into());
    a.waves[3].enabled = 1;
    a.waves[3].per_point = "x=1;\u{1}".into();
    a.shapes[7].enabled = 1;
    a.extra.push(("custom_key".into(), "some value".into()));
    let t = a.to_milk();
    let b = Preset::parse(t.as_bytes()).unwrap();
    assert_eq!(a, b);
    assert_eq!(t, b.to_milk());
}

fn pm(s: &str) -> Preset {
    Preset::parse_with(s.as_bytes(), Flavor::ProjectM).unwrap()
}

/// BrainStain presets carry a stray `per_frame_8 ` line (no `=`, an empty value) far above the
/// real `per_frame_8=...`.  MD2 finds the real line through its cursor; projectM's map keeps the
/// first copy only, so that equation never runs (oracle `zoom` stays at its preset value).
#[test]
fn projectm_first_duplicate_wins_even_when_empty() {
    let src = "per_frame_8 \nper_frame_1=a=1;\nper_frame_7=g=7;\nper_frame_8=zoom=zoom+1;\nper_frame_9=h=9;\n";
    // `per_frame_2` is missing in both: the block ends after line 1
    assert_eq!(p(src).per_frame, "a=1;\u{1}");
    let src =
        "per_frame_1=a=1;\nper_frame_2=b=2;\nper_frame_3 \nper_frame_4=d=4;\nper_frame_3=c=3;\n";
    assert_eq!(pm(src).per_frame, "a=1;\u{1}b=2;\u{1}\u{1}d=4;\u{1}");
    // MD2: line 3 is found through the cursor-or-search of `per_frame_3`: first copy, the empty one
    assert_eq!(p(src).per_frame, "a=1;\u{1}b=2;\u{1}\u{1}d=4;\u{1}");
    // MD2's cursor rescues the real line when it sits right after the previous hit
    let src = "per_frame_2 \nper_frame_1=a=1;\nper_frame_2=b=2;\n";
    assert_eq!(p(src).per_frame, "a=1;\u{1}b=2;\u{1}");
    assert_eq!(pm(src).per_frame, "a=1;\u{1}\u{1}");
}

#[test]
fn projectm_keys_are_case_insensitive_and_values_use_stoi_stof() {
    let d = pm(
        "FDECAY=0.5\nnWaveMode=3.9\nzoom=1e\nrot=1e50\nfWaveScale=0x1p1\nbBrighten=-1\nbDarken=2\n",
    );
    assert_eq!(d.base.decay, 0.5);
    assert_eq!(d.base.wave_mode, 3);
    assert_eq!(d.base.zoom, 1.0); // `1e` -> 1 (sscanf would fail and keep the default 1.0 anyway)
    assert_eq!(d.base.rot, 0.0); // out of range -> default
    assert_eq!(d.base.wave_scale, 2.0);
    assert!(!d.base.brighten); // GetBool: > 0
    assert!(d.base.darken);
    let m = p("fdecay=0.5\nbBrighten=-1\n");
    assert_eq!(m.base.decay, 0.98); // MD2 keys are case-sensitive
    assert!(m.base.brighten);
}

#[test]
fn projectm_defaults_for_missing_keys() {
    let d = pm("fDecay=0.9\nshapecode_0_enabled=1\n");
    assert_eq!((d.base.wave_r, d.base.wave_x), (1.0, 0.5));
    assert_eq!(d.shapes[0].border_a, 0.0);
    let m = p("fDecay=0.9\nshapecode_0_enabled=1\n");
    assert_eq!((m.base.wave_r, m.base.wave_x), (0.0, 0.0));
    assert_eq!(m.shapes[0].border_a, 0.1);
}

/// `shapecode_N_image` (`CustomShape.cpp:59`): a projectM addition; the MD2 importer has no such key.
#[test]
fn projectm_shape_image_key() {
    let src = "shapecode_1_enabled=1\nshapecode_1_textured=1\nshapecode_1_image=fc_clouds2\n";
    let d = pm(src);
    assert_eq!(d.shapes[1].image, "fc_clouds2");
    assert!(
        d.extra.iter().all(|(k, _)| k != "shapecode_1_image"),
        "consumed, not left in extra"
    );
    assert_eq!(pm("shapecode_1_enabled=1\n").shapes[1].image, "");
    assert_eq!(pm("SHAPECODE_1_IMAGE=Foo\n").shapes[1].image, "Foo"); // keys are case-insensitive there
    // MD2: ignored by the model and kept verbatim in `extra`
    let m = p(src);
    assert_eq!(m.shapes[1].image, "");
    assert!(
        m.extra
            .contains(&("shapecode_1_image".to_string(), "fc_clouds2".to_string()))
    );
    // serialisation round-trips in both flavours
    let t = d.to_milk();
    assert!(t.contains("shapecode_1_image=fc_clouds2"));
    assert_eq!(
        Preset::parse_with(t.as_bytes(), Flavor::ProjectM).unwrap(),
        d
    );
    assert_eq!(Preset::parse(m.to_milk().as_bytes()).unwrap(), m);
}

#[test]
fn projectm_flavour_rejects_unreadable_files() {
    use lactea_preset::Flavor;
    let pm = |b: &[u8]| Preset::parse_with(b, Flavor::ProjectM);
    assert_eq!(pm(b"zoom=1.0\nbad\0\n"), Err(ParseError::BinaryData));
    assert_eq!(pm(b"no_separator_lines\n\n"), Err(ParseError::NoKeys));
    let mut big = b"zoom=1.0\n".to_vec();
    big.resize(0x100001, b'\n');
    assert_eq!(pm(&big), Err(ParseError::TooLarge));
    // MD2 truncates at NUL instead of failing
    assert!(Preset::parse(b"zoom=1.0\nbad\0\n").is_ok());
}
