//! `.wave` / `.shape` files and the MD3 `.milk` writer (docs/md3.md). Fixtures are hand-written.

use lactea_preset::*;

/// A lasso-style wave: one 3-D point projected with a perspective divide.
const WAVE: &str = "wavecode_0_enabled=1\n\
wavecode_0_samples=200\n\
wavecode_0_sep=0\n\
wavecode_0_bSpectrum=0\n\
wavecode_0_bUseDots=0\n\
wavecode_0_bDrawThick=1\n\
wavecode_0_bAdditive=1\n\
wavecode_0_scaling=1.00000\n\
wavecode_0_smoothing=0.50000\n\
wavecode_0_r=1.000\n\
wavecode_0_g=0.500\n\
wavecode_0_b=0.250\n\
wavecode_0_a=0.800\n\
wave_0_init1=t1=0;\n\
wave_0_per_frame1=t1=t1+0.01;\n\
wave_0_per_frame2=zoomz=2+sin(time);\n\
wave_0_per_point1=x3=cos(sample*6.28);y3=sin(sample*6.28);z3=1+sample;\n\
wave_0_per_point2=x=0.5+x3/(z3+zoomz);\n\
wave_0_per_point3=y=0.5+y3/(z3+zoomz);\n";

const SHAPE: &str = "shapecode_0_enabled=1\n\
shapecode_0_sides=6\n\
shapecode_0_additive=0\n\
shapecode_0_thickOutline=1\n\
shapecode_0_textured=0\n\
shapecode_0_num_inst=3\n\
shapecode_0_x=0.250\n\
shapecode_0_y=0.750\n\
shapecode_0_rad=0.12500\n\
shapecode_0_ang=1.50000\n\
shapecode_0_tex_ang=0.00000\n\
shapecode_0_tex_zoom=1.00000\n\
shapecode_0_r=1.000\n\
shapecode_0_g=0.000\n\
shapecode_0_b=0.000\n\
shapecode_0_a=1.000\n\
shapecode_0_r2=0.000\n\
shapecode_0_g2=1.000\n\
shapecode_0_b2=0.000\n\
shapecode_0_a2=0.500\n\
shapecode_0_border_r=1.000\n\
shapecode_0_border_g=1.000\n\
shapecode_0_border_b=1.000\n\
shapecode_0_border_a=0.100\n\
shape_0_init1=q1=0;\n\
shape_0_per_frame1=ang=ang+0.1;\n\
shape_0_per_frame2=rad=0.1+0.05*sin(time);\n";

#[test]
fn wave_import_export_roundtrip() {
    let mut p = Preset::default();
    import_wave_file(&mut p, 2, WAVE.as_bytes()).unwrap();
    let w = &p.waves[2];
    assert_eq!(
        (w.enabled, w.samples, w.b_draw_thick, w.b_additive),
        (1, 200, 1, 1)
    );
    assert_eq!((w.scaling, w.smoothing, w.g), (1.0, 0.5, 0.5));
    assert_eq!(code_lines(&w.per_point).count(), 3);
    assert_eq!(code_lines(&w.init).collect::<Vec<_>>(), ["t1=0;"]);
    // the file text is exactly what the importer was given
    assert_eq!(export_wave_file(&p, 2).unwrap(), WAVE);

    // export slot 2, import into slot 9 of a default preset: same slot
    let mut q = Preset::default();
    import_wave_file(&mut q, 9, export_wave_file(&p, 2).unwrap().as_bytes()).unwrap();
    assert_eq!(q.waves[9], p.waves[2]);
    assert_eq!(q.waves[2], CustomWave::default());
}

#[test]
fn wave_export_is_truncated_to_printed_precision() {
    let mut p = Preset::default();
    p.waves[0].enabled = 1;
    p.waves[0].r = 0.123_456;
    p.waves[0].smoothing = 0.123_456_7;
    let t = export_wave_file(&p, 0).unwrap();
    assert!(t.contains("wavecode_0_r=0.123\n"), "{t}");
    assert!(t.contains("wavecode_0_smoothing=0.12346\n"), "{t}");
    assert!(!t.contains('['));
    assert!(!t.contains('\r'));
}

#[test]
fn shape_import_export_roundtrip() {
    let mut p = Preset::default();
    import_shape_file(&mut p, 5, SHAPE.as_bytes()).unwrap();
    let s = &p.shapes[5];
    assert_eq!((s.sides, s.instances, s.thick_outline), (6, 3, 1));
    assert_eq!(
        (s.x, s.y, s.rad, s.ang, s.a2),
        (0.25, 0.75, 0.125, 1.5, 0.5)
    );
    assert_eq!(code_lines(&s.per_frame).count(), 2);
    assert_eq!(export_shape_file(&p, 5).unwrap(), SHAPE);
    let mut q = Preset::default();
    import_shape_file(&mut q, 0, export_shape_file(&p, 5).unwrap().as_bytes()).unwrap();
    assert_eq!(q.shapes[0], p.shapes[5]);
}

#[test]
fn disabled_shape_without_code() {
    let mut p = Preset::default();
    p.shapes[3].enabled = 1;
    p.shapes[3].init = "q1=1;\u{1}".into();
    import_shape_file(&mut p, 3, b"shapecode_0_enabled=0\n").unwrap();
    // enabled is overwritten, other values keep, code cleared
    assert_eq!(p.shapes[3].enabled, 0);
    assert_eq!(p.shapes[3].sides, 4);
    assert!(p.shapes[3].init.is_empty());
    let t = export_shape_file(&p, 3).unwrap();
    assert!(t.starts_with("shapecode_0_enabled=0\n"));
    assert!(!t.contains("shape_0_"));
}

#[test]
fn wave_import_keeps_missing_keys_and_replaces_code() {
    let mut p = Preset::default();
    p.waves[4].a = 0.7;
    p.waves[4].init = "old=1;\u{1}".into();
    p.waves[4].per_frame = "old=2;\u{1}".into();
    p.waves[4].per_point = "old=3;\u{1}".into();
    import_wave_file(
        &mut p,
        4,
        b"wavecode_0_enabled=1\nwavecode_0_samples=200\nwavecode_0_r=1\n",
    )
    .unwrap();
    let w = &p.waves[4];
    assert_eq!((w.enabled, w.samples, w.r, w.a), (1, 200, 1.0, 0.7));
    assert!(w.init.is_empty() && w.per_frame.is_empty() && w.per_point.is_empty());
}

#[test]
fn import_errors() {
    let mut p = Preset::default();
    let before = p.clone();
    assert_eq!(
        import_wave_file(&mut p, 16, WAVE.as_bytes()),
        Err(ParseError::SlotOutOfRange {
            slot: 16,
            count: 16
        })
    );
    assert_eq!(
        import_shape_file(&mut p, 99, SHAPE.as_bytes()),
        Err(ParseError::SlotOutOfRange {
            slot: 99,
            count: 16
        })
    );
    assert_eq!(import_wave_file(&mut p, 0, b""), Err(ParseError::Empty));
    assert_eq!(
        import_shape_file(&mut p, 0, &[0xFF, 0xFE, b'a', 0]),
        Err(ParseError::Utf16)
    );
    assert_eq!(p, before);
    assert!(export_wave_file(&p, 16).is_none());
    assert!(export_shape_file(&p, 16).is_none());
    assert!(
        ParseError::SlotOutOfRange {
            slot: 16,
            count: 16
        }
        .to_string()
        .contains("16")
    );
}

#[test]
fn wave_import_uses_first_duplicate_and_crlf() {
    let mut p = Preset::default();
    import_wave_file(
        &mut p,
        0,
        b"wavecode_0_enabled=1\r\nwavecode_0_samples=64\r\nwavecode_0_samples=32\r\n",
    )
    .unwrap();
    assert_eq!(p.waves[0].samples, 64);
}

// ---- MD3 .milk writer -----------------------------------------------------------------

fn lines(s: &str) -> Vec<&str> {
    s.lines().collect()
}

#[test]
fn md3_writer_layout() {
    let mut p = Preset::default();
    p.milkdrop_preset_version = 201;
    p.warp_ps_version = 2;
    p.comp_ps_version = 3;
    p.base.fft_attack = 0.25;
    p.base.zoom = 1.012_345_6;
    p.per_frame = "zoom=zoom+0.1;\u{1}".into();
    let t = p.to_milk_md3();
    assert!(!t.contains('\r'));
    let l = lines(&t);
    assert_eq!(
        &l[..6],
        &[
            "MILKDROP_PRESET_VERSION=201",
            "PSVERSION=3",
            "PSVERSION_WARP=2",
            "PSVERSION_COMP=3",
            "[preset00]",
            "FFTAttack=0.250"
        ]
    );
    assert_eq!(l[6], "FFTDecay=0.700");
    assert!(t.contains("\nzoom=1.01235\n"));
    assert!(t.contains("\nfZoomExponent=1.00000\n"));
    assert!(t.contains("\nper_frame_1=zoom=zoom+0.1;\n"));
    // no banner / comment lines
    assert!(l.iter().all(|x| x.contains('=') || x.starts_with('[')));
}

#[test]
fn md3_writer_ps0_has_no_version_lines() {
    let t = Preset::default().to_milk_md3();
    assert!(t.starts_with("[preset00]\nFFTAttack=0.500\nFFTDecay=0.700\n"));
    assert!(!t.contains("PSVERSION"));
    assert!(!t.contains("MILKDROP_PRESET_VERSION"));
}

/// A preset with a sprite is written as version 300 followed by the sprite block in MD3's layout (blank line,
/// `%.6f` scalars, unpadded `0x%X` key except the literal `0x000000`), and reads back identically.
#[test]
fn md3_writer_emits_version_300_and_the_sprite_block() {
    let mut p = Preset::default();
    p.warp_ps_version = 2;
    p.comp_ps_version = 2;
    assert!(p.to_milk_md3().starts_with("MILKDROP_PRESET_VERSION=201\n"));
    assert!(!p.to_milk_md3().contains("SPRITE1"));

    p.sprite = Some(PresetSprite {
        name: "sprites\\a.png".into(),
        color_key: 0xFF,
        layer: 1,
        blend: 7,
        alpha: 0.5,
        burn: 0,
        x: 0.25,
        y: -0.125,
        sx: -0.8,
        sy: -0.6,
        rot_mode: 1.0,
        speed: 1.5,
        repeat_x: 1.0,
        repeat_y: 0.95,
        code: "x=0.5+0.1*sin(time);\u{1}a=0.8;\u{1}".into(),
    });
    let t = p.to_milk_md3();
    assert!(t.starts_with("MILKDROP_PRESET_VERSION=300\n"));
    let block = "\n[SPRITE1_BEGIN]\nSpriteName=sprites\\a.png\nSpriteColorKey=0xFF\nSpriteLayer=1\nSpriteBlend=7\n\
SpriteAlpha=0.500000\nSpriteBurn=0\nSpriteX=0.250000\nSpriteY=-0.125000\nSpriteSX=-0.800000\nSpriteSY=-0.600000\n\
SpriteRot=1.000000\nSpriteSpeed=1.500000\nSpriteRepeatX=1.000000\nSpriteRepeatY=0.950000\n\
code_1=x=0.5+0.1*sin(time);\ncode_2=a=0.8;\n[SPRITE1_END]\n";
    assert!(t.ends_with(block), "{t}");

    let back = Preset::parse(t.as_bytes()).unwrap().sprite.unwrap();
    assert_eq!(back, p.sprite.clone().unwrap());
    p.sprite.as_mut().unwrap().color_key = 0;
    assert!(p.to_milk_md3().contains("SpriteColorKey=0x000000\n"));
}

#[test]
fn md3_writer_renumbers_enabled_slots() {
    let mut p = Preset::default();
    for (slot, en) in [(1, 1), (5, 1), (7, 2), (9, 0)] {
        p.waves[slot].enabled = en;
    }
    p.waves[1].samples = 11;
    p.waves[5].samples = 55;
    p.waves[7].samples = 77;
    for (slot, en) in [(0, 1), (3, 1), (4, 2)] {
        p.shapes[slot].enabled = en;
    }
    p.shapes[0].sides = 5;
    p.shapes[3].sides = 8;
    p.shapes[3].per_frame = "ang=1;\u{1}".into();
    p.waves[5].per_point = "x=0.5;\u{1}y=0.5;\u{1}".into();
    let t = p.to_milk_md3();
    assert!(t.contains("\nwavecode_0_samples=11\n"));
    assert!(t.contains("\nwavecode_1_samples=55\n"));
    assert!(!t.contains("wavecode_2_"));
    assert!(t.contains("\nwave_1_per_point2=y=0.5;\n"));
    assert!(t.contains("\nshapecode_0_sides=5\n"));
    assert!(t.contains("\nshapecode_1_sides=8\n"));
    assert!(t.contains("\nshape_1_per_frame1=ang=1;\n"));
    assert!(!t.contains("shapecode_2_"));
    // waves precede shapes precede code blocks
    let pos = |k: &str| t.find(k).unwrap();
    assert!(pos("wavecode_1_a=") < pos("shapecode_0_enabled"));
    assert!(pos("wave_1_per_point2") < pos("shapecode_0_enabled"));

    let q = Preset::parse(t.as_bytes()).unwrap();
    assert_eq!((q.waves[0].samples, q.waves[1].samples), (11, 55));
    assert_eq!((q.shapes[0].sides, q.shapes[1].sides), (5, 8));
    assert_eq!(q.waves[2].enabled, 0);
}

#[test]
fn md3_writer_backticks_shader_lines_for_ps2_only() {
    let mut p = Preset::default();
    p.warp_ps_version = 2;
    p.comp_ps_version = 1;
    p.warp_shader = Some("shader_body\u{1}{\u{1}ret=1;\u{1}}\u{1}".into());
    p.comp_shader = Some("shader_body\u{1}".into());
    let t = p.to_milk_md3();
    assert!(t.contains("\nwarp_1=`shader_body\n"));
    assert!(t.contains("\nwarp_3=`ret=1;\n"));
    assert!(!t.contains("comp_1"));
    p.comp_ps_version = 2;
    assert!(p.to_milk_md3().contains("\ncomp_1=`shader_body\n"));
}

#[test]
fn md3_writer_md31_handling() {
    let mut p = Preset::default();
    p.warp_ps_version = 5;
    p.comp_ps_version = 5;
    p.warp_shader = Some("// This shader works only with MD3\u{1}ret=0;\u{1}".into());
    p.comp_shader = Some("ret=0;\u{1}".into());
    p.extra.push(("MD31".into(), "18446744073709551615".into()));
    p.extra.push(("MD32".into(), "42".into()));
    let t = p.to_milk_md3();
    // MD31 first; MD32 omitted: the comp text has no marker
    assert!(t.starts_with("MD31=18446744073709551615\nMILKDROP_PRESET_VERSION=201\n"));
    assert!(!t.contains("MD32"));
    p.comp_shader = Some("// This shader works only with MD3\u{1}".into());
    let t = p.to_milk_md3();
    assert!(t.starts_with("MD31=18446744073709551615\nMD32=42\n"));
    assert_eq!(t.matches("MD31").count(), 1);
    // marker without handle: nothing
    p.extra.clear();
    assert!(p.to_milk_md3().starts_with("MILKDROP_PRESET_VERSION=201\n"));
}

#[test]
fn md3_writer_reparses_to_same_values() {
    let src = format!(
        "MD31=7\nMILKDROP_PRESET_VERSION=201\nPSVERSION=2\nPSVERSION_WARP=2\nPSVERSION_COMP=2\n[preset00]\n\
         fDecay=0.9\nzoom=1.02\nnWaveMode=3\nbWaveThick=1\nwave_r=0.5\nwave_g=0.25\nwave_b=1\nob_a=0.5\n\
         per_frame_1=q1=1;\nper_pixel_1=zoom=zoom+rad*0.01;\n\
         warp_1=`shader_body\nwarp_2=`{{ ret=tex2D(sampler_main,uv).xyz; }}\n\
         comp_1=`// This shader works only with MD3\ncomp_2=`shader_body\n{WAVE}{SHAPE}"
    );
    let p = Preset::parse(src.as_bytes()).unwrap();
    assert!(p.extra.iter().any(|(k, _)| k == "MD31"));
    let q = Preset::parse(p.to_milk_md3().as_bytes()).unwrap();
    assert_eq!(q.base, p.base);
    assert_eq!(q.waves[0], p.waves[0]);
    assert_eq!(q.shapes[0], p.shapes[0]);
    assert_eq!(q.per_frame, p.per_frame);
    assert_eq!(q.per_pixel, p.per_pixel);
    assert_eq!(q.warp_shader, p.warp_shader);
    assert_eq!(q.comp_shader, p.comp_shader);
    assert_eq!((q.warp_ps_version, q.comp_ps_version), (2, 2));
    // `to_milk` is untouched: lossless
    assert_eq!(Preset::parse(p.to_milk().as_bytes()).unwrap(), p);
}
