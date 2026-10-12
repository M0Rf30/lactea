//! Which MilkDrop family a preset file was written for, judged from its bytes alone.
//!
//! The default reference is projectM; only *positive* evidence of a MilkDrop 3 feature selects MilkDrop 3.
//! MilkDrop 2 and BeatDrop files carry no marker that tells them from projectM files (same keys, same code, same
//! shaders), so [`DetectedFamily`] has no MilkDrop 2 variant and a plain `.milk` always resolves to
//! [`DetectedFamily::ProjectM`], the reference behaviour.
//!
//! Every heuristic is listed in `docs/md3.md` ("Automatic compatibility"). What is deliberately **not** evidence,
//! because MilkDrop 2 / BeatDrop files have it too:
//! - `nWaveMode` 9..=17 and enabled custom waves / shapes in slots 4..=15 (BeatDrop raised both limits);
//! - `MILKDROP_PRESET_VERSION` 200 / 201 and every `PSVERSION*`;
//! - BeatDrop's `get_fft_peak*` / `get_wave*` shader helpers and `mousex` / `mousey` expression variables
//!   (MilkDrop 3 has none of them).

use crate::{MAX_CUSTOM_SHAPES, MAX_CUSTOM_WAVES, Preset, milk2};
use std::fmt;

/// The preset family a file belongs to, as far as its bytes tell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DetectedFamily {
    /// Nothing MilkDrop-3-specific found: projectM, the reference (MilkDrop 2 files land here too).
    #[default]
    ProjectM,
    /// At least one [`Evidence`] only MilkDrop 3 files carry.
    MilkDrop3,
}

/// One reason a preset needs MilkDrop 3.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Evidence {
    /// A `.milk2` double preset (the four `[PRESET1_BEGIN]` .. `[PRESET2_END]` markers,
    /// [`milk2::is_double_preset`]).
    Milk2Container,
    /// A key of MilkDrop 3's encrypted shaders (`MD31=` / `MD32=`).
    EncryptedShaderKey(&'static str),
    /// A `[SPRITE1_BEGIN]` block or a non-empty `SpriteName=`.
    SpriteBlock,
    /// `MILKDROP_PRESET_VERSION=300`, the version MilkDrop 3 writes for presets with a sprite.
    PresetVersion(i32),
    /// An enabled custom shape with more than 100 sides (MilkDrop 2 / projectM draw at most 100, MilkDrop 3 500).
    ShapeSides {
        /// Shape slot.
        shape: usize,
        /// The `sides` value.
        sides: i32,
    },
    /// A shader identifier only the MilkDrop 3 prelude defines (`get_fft`, `sampler_fft`, `mouse`, `_c14`,
    /// `q33`..`q64`, ...).
    ShaderName {
        /// `"warp"` or `"comp"`.
        stage: &'static str,
        /// The identifier as written.
        name: String,
    },
    /// An expression variable only MilkDrop 3 has (`q33`..`q64`; `t9`..`t16` in custom wave / shape code).
    ExpressionVariable {
        /// Which code part, e.g. `"per_frame"`, `"wave 3 per_point"`.
        part: String,
        /// The variable as written.
        name: String,
    },
}

impl fmt::Display for Evidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Evidence::Milk2Container => f.write_str(".milk2 double preset"),
            Evidence::EncryptedShaderKey(k) => write!(f, "encrypted shader key {k}"),
            Evidence::SpriteBlock => f.write_str("sprite block"),
            Evidence::PresetVersion(v) => write!(f, "MILKDROP_PRESET_VERSION={v}"),
            Evidence::ShapeSides { shape, sides } => {
                write!(f, "shape {shape} with {sides} sides")
            }
            Evidence::ShaderName { stage, name } => write!(f, "{stage} shader uses {name}"),
            Evidence::ExpressionVariable { part, name } => write!(f, "{part} uses {name}"),
        }
    }
}

/// The result of [`detect_compat`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DetectedCompat {
    /// [`DetectedFamily::MilkDrop3`] iff `evidence` is not empty.
    pub family: DetectedFamily,
    /// Every heuristic that fired, in a fixed order (container, keys, sprite, version, shapes, warp shader, composite
    /// shader, expression parts); an identifier is listed once per shader / code part.
    pub evidence: Vec<Evidence>,
}

/// Decide which family `bytes` (a `.milk` or `.milk2` file) was written for. Pure and deterministic; a file that
/// does not parse as a preset has no evidence and resolves to [`DetectedFamily::ProjectM`] (loading it fails
/// anyway).
pub fn detect_compat(bytes: &[u8]) -> DetectedCompat {
    let mut evidence = Vec::new();
    if milk2::is_double_preset(bytes) {
        // both halves are MilkDrop 3 presets by construction; nothing else to learn from them
        evidence.push(Evidence::Milk2Container);
        return DetectedCompat {
            family: DetectedFamily::MilkDrop3,
            evidence,
        };
    }
    scan_lines(bytes, &mut evidence);
    if let Ok(p) = Preset::parse(bytes) {
        scan_preset(&p, &mut evidence);
    }
    DetectedCompat {
        family: if evidence.is_empty() {
            DetectedFamily::ProjectM
        } else {
            DetectedFamily::MilkDrop3
        },
        evidence,
    }
}

/// Raw-text evidence (independent of how the importer reads the keys): encrypted shader keys and the sprite header.
fn scan_lines(bytes: &[u8], out: &mut Vec<Evidence>) {
    let (mut md31, mut md32, mut sprite) = (false, false, false);
    for line in bytes.split(|&b| b == b'\n') {
        let line = line.trim_ascii();
        if line.starts_with(b"MD31=") && !md31 {
            md31 = true;
            out.push(Evidence::EncryptedShaderKey("MD31"));
        } else if line.starts_with(b"MD32=") && !md32 {
            md32 = true;
            out.push(Evidence::EncryptedShaderKey("MD32"));
        } else if line == b"[SPRITE1_BEGIN]" && !sprite {
            sprite = true;
            out.push(Evidence::SpriteBlock);
        }
    }
}

fn scan_preset(p: &Preset, out: &mut Vec<Evidence>) {
    // a `SpriteName=` outside a block header still loads a sprite
    if p.sprite.is_some() && !out.contains(&Evidence::SpriteBlock) {
        out.push(Evidence::SpriteBlock);
    }
    if p.milkdrop_preset_version == 300 {
        out.push(Evidence::PresetVersion(300));
    }
    for (i, s) in p.shapes.iter().enumerate().take(MAX_CUSTOM_SHAPES) {
        if s.enabled != 0 && s.sides > 100 {
            out.push(Evidence::ShapeSides {
                shape: i,
                sides: s.sides,
            });
        }
    }
    for (stage, text) in [("warp", &p.warp_shader), ("comp", &p.comp_shader)] {
        if let Some(text) = text {
            shader_names(text, stage, out);
        }
    }
    // `t9..t16` exist only in MilkDrop 3's custom wave / shape VMs; elsewhere `t9` is an ordinary variable name
    let mut code = |part: String, text: &str, t_vars: bool| {
        expression_variables(text, part, t_vars, out);
    };
    code("per_frame_init".into(), &p.per_frame_init, false);
    code("per_frame".into(), &p.per_frame, false);
    code("per_pixel".into(), &p.per_pixel, false);
    for (i, w) in p.waves.iter().enumerate().take(MAX_CUSTOM_WAVES) {
        code(format!("wave {i} init"), &w.init, true);
        code(format!("wave {i} per_frame"), &w.per_frame, true);
        code(format!("wave {i} per_point"), &w.per_point, true);
    }
    for (i, s) in p.shapes.iter().enumerate().take(MAX_CUSTOM_SHAPES) {
        code(format!("shape {i} init"), &s.init, true);
        code(format!("shape {i} per_frame"), &s.per_frame, true);
    }
    if let Some(sp) = &p.sprite {
        code("sprite".into(), &sp.code, false);
    }
}

/// `qN` with `N` in 33..=64 or, with `with_t`, `tN` with `N` in 9..=16, spelled canonically (`q033`, `q330`,
/// `q65` and `t17` are other names).
fn is_high_var(name: &str, with_t: bool) -> bool {
    let range = match name.as_bytes().first() {
        Some(b'q' | b'Q') => 33..=64,
        Some(b't' | b'T') if with_t => 9..=16,
        _ => return false,
    };
    let digits = &name[1..];
    digits
        .parse::<u8>()
        .is_ok_and(|n| range.contains(&n) && digits == n.to_string())
}

/// Identifiers only the MilkDrop 3 shader prelude defines (`lactea-hlsl`, `md3_wrapped_hlsl`). Case-sensitive, like
/// HLSL.
fn is_md3_shader_name(name: &str) -> bool {
    const FIXED: [&str; 18] = [
        "get_fft",
        "get_fft_hz",
        "sampler_fft",
        "mouse",
        "mouse_x",
        "mouse_y",
        "mouse_z",
        "mouse_w",
        "mouse_pos",
        "_c14",
        "_qi",
        "_qj",
        "_qk",
        "_ql",
        "_qm",
        "_qn",
        "_qo",
        "_qp",
    ];
    if FIXED.contains(&name) {
        return true;
    }
    // `sampler_fw_fft`, `sampler_pc_fft`, ...: the quality prefixes of a sampler name
    if let Some(rest) = name.strip_prefix("sampler_")
        && let Some(base) = rest.get(3..)
        && matches!(&rest[..3], "fw_" | "fc_" | "pw_" | "pc_")
    {
        return base == "fft";
    }
    // `q33..q64` are lower case in shaders (the macros are `#define`d in lower case)
    name.starts_with('q') && is_high_var(name, false)
}

/// Words after which an identifier is being declared (or `#define`d), not used.
fn declares(prev: &str) -> bool {
    matches!(
        prev,
        "float"
            | "float2"
            | "float3"
            | "float4"
            | "float2x2"
            | "float3x3"
            | "float4x4"
            | "float3x4"
            | "float4x3"
            | "half"
            | "half2"
            | "half3"
            | "half4"
            | "int"
            | "int2"
            | "int3"
            | "int4"
            | "uint"
            | "bool"
            | "double"
            | "define"
            | "sampler"
            | "sampler2D"
            | "sampler3D"
            | "struct"
    )
}

fn shader_names(text: &str, stage: &'static str, out: &mut Vec<Evidence>) {
    let mut seen: Vec<(&str, bool)> = Vec::new();
    for tok in Tokens::new(text, false) {
        if tok.after_dot || !is_md3_shader_name(tok.name) {
            continue;
        }
        let declared = tok.prev.is_some_and(declares);
        match seen.iter_mut().find(|(n, _)| *n == tok.name) {
            Some((_, d)) => *d |= declared,
            None => seen.push((tok.name, declared)),
        }
    }
    // a preset that declares the name itself (a local `float2 mouse`, its own `get_fft`) defines it, it does not use
    // the prelude's
    out.extend(
        seen.into_iter()
            .filter(|(_, declared)| !declared)
            .map(|(name, _)| Evidence::ShaderName {
                stage,
                name: name.to_owned(),
            }),
    );
}

fn expression_variables(text: &str, part: String, t_vars: bool, out: &mut Vec<Evidence>) {
    let mut seen: Vec<&str> = Vec::new();
    for tok in Tokens::new(text, true) {
        if !tok.after_dot && is_high_var(tok.name, t_vars) && !seen.contains(&tok.name) {
            seen.push(tok.name);
            out.push(Evidence::ExpressionVariable {
                part: part.clone(),
                name: tok.name.to_owned(),
            });
        }
    }
}

/// One identifier of a source text.
struct Token<'a> {
    name: &'a str,
    /// Right after a `.` (a member, not a global).
    after_dot: bool,
    /// The identifier directly before this one, if only blanks lie between them.
    prev: Option<&'a str>,
}

/// Identifiers of C-like (shader) or EEL source, skipping comments and number literals. A joined code block
/// separates its lines with `\u{1}`, which ends a line comment like `\n`.
struct Tokens<'a> {
    src: &'a str,
    i: usize,
    eel: bool,
    after_dot: bool,
    prev: Option<&'a str>,
}

impl<'a> Tokens<'a> {
    fn new(src: &'a str, eel: bool) -> Self {
        Tokens {
            src,
            i: 0,
            eel,
            after_dot: false,
            prev: None,
        }
    }
}

impl<'a> Iterator for Tokens<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Token<'a>> {
        let b = self.src.as_bytes();
        let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
        while self.i < b.len() {
            let c = b[self.i];
            let n = b.get(self.i + 1).copied();
            let start = self.i;
            if (c == b'/' && n == Some(b'/')) || (self.eel && c == b'\\' && n == Some(b'\\')) {
                while self.i < b.len() && !matches!(b[self.i], b'\n' | b'\r' | 1) {
                    self.i += 1;
                }
            } else if c == b'/' && n == Some(b'*') {
                self.i = self.src[start + 2..]
                    .find("*/")
                    .map_or(b.len(), |e| start + 2 + e + 2);
            } else if c.is_ascii_digit() || (c == b'.' && n.is_some_and(|d| d.is_ascii_digit())) {
                // number literal: `1e5`, `0x1F`, `.5`; its letters are not identifiers
                while self.i < b.len() && (is_word(b[self.i]) || b[self.i] == b'.') {
                    self.i += 1;
                }
                (self.after_dot, self.prev) = (false, None);
            } else if c.is_ascii_alphabetic() || c == b'_' {
                while self.i < b.len() && is_word(b[self.i]) {
                    self.i += 1;
                }
                let name = &self.src[start..self.i];
                let tok = Token {
                    name,
                    after_dot: self.after_dot,
                    prev: self.prev,
                };
                (self.after_dot, self.prev) = (false, Some(name));
                return Some(tok);
            } else if c == b'.' {
                self.i += 1;
                (self.after_dot, self.prev) = (true, None);
            } else {
                self.i += 1;
                if !c.is_ascii_whitespace() && c != 1 {
                    (self.after_dot, self.prev) = (false, None);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn family(text: &str) -> DetectedFamily {
        detect_compat(text.as_bytes()).family
    }

    fn evidence(text: &str) -> Vec<Evidence> {
        detect_compat(text.as_bytes()).evidence
    }

    /// A shader line the way the importer stores it (`\u{1}` between lines).
    fn comp(lines: &[&str]) -> String {
        lines
            .iter()
            .enumerate()
            .map(|(i, l)| format!("comp_{}=`{l}\n", i + 1))
            .collect()
    }

    #[test]
    fn an_empty_or_plain_preset_is_projectm() {
        assert_eq!(detect_compat(b"").family, DetectedFamily::ProjectM);
        let d = detect_compat(b"MILKDROP_PRESET_VERSION=201\nPSVERSION=2\n[preset00]\nzoom=1\n");
        assert_eq!(d, DetectedCompat::default());
        assert!(d.evidence.is_empty());
    }

    #[test]
    fn a_double_preset_container_is_milkdrop3() {
        let text = "[presets]\n[PRESET1_BEGIN]\nzoom=1\n[PRESET1_END]\n[PRESET2_BEGIN]\nzoom=1\n[PRESET2_END]\n";
        let d = detect_compat(text.as_bytes());
        assert_eq!(d.family, DetectedFamily::MilkDrop3);
        assert_eq!(d.evidence, [Evidence::Milk2Container]);
        // an incomplete marker set is not a container
        let half = "[PRESET1_BEGIN]\nzoom=1\n[PRESET1_END]\n";
        assert_eq!(family(half), DetectedFamily::ProjectM);
    }

    #[test]
    fn encrypted_shader_keys() {
        let ev = evidence("MD31=abc\nMD32=def\n[preset00]\nzoom=1\n");
        assert_eq!(
            ev,
            [
                Evidence::EncryptedShaderKey("MD31"),
                Evidence::EncryptedShaderKey("MD32")
            ]
        );
        // other keys that merely start alike
        assert_eq!(
            family("MD310=1\nMD3=1\nmd31=1\nxMD31=1\n"),
            DetectedFamily::ProjectM
        );
    }

    #[test]
    fn sprite_block_and_version() {
        let sprite = "MILKDROP_PRESET_VERSION=300\n[preset00]\nzoom=1\n[SPRITE1_BEGIN]\nSpriteName=sprites\\a.png\n[SPRITE1_END]\n";
        let ev = evidence(sprite);
        assert_eq!(ev, [Evidence::SpriteBlock, Evidence::PresetVersion(300)]);
        // a header with an empty name still marks the file
        assert_eq!(
            evidence("[SPRITE1_BEGIN]\nSpriteName=\n[SPRITE1_END]\n"),
            [Evidence::SpriteBlock]
        );
        // a name outside a block header loads a sprite as well
        assert_eq!(evidence("SpriteName=x.png\n"), [Evidence::SpriteBlock]);
        // the ordinary MD2 versions are not evidence
        for v in [100, 200, 201, 299, 301] {
            assert_eq!(
                family(&format!("MILKDROP_PRESET_VERSION={v}\n[preset00]\n")),
                DetectedFamily::ProjectM,
                "{v}"
            );
        }
    }

    #[test]
    fn shape_sides_above_100_only_when_enabled() {
        assert_eq!(
            evidence("[preset00]\nshapecode_3_enabled=1\nshapecode_3_sides=200\n"),
            [Evidence::ShapeSides {
                shape: 3,
                sides: 200
            }]
        );
        assert_eq!(
            family("[preset00]\nshapecode_3_enabled=1\nshapecode_3_sides=100\n"),
            DetectedFamily::ProjectM
        );
        assert_eq!(
            family("[preset00]\nshapecode_3_enabled=0\nshapecode_3_sides=200\n"),
            DetectedFamily::ProjectM
        );
    }

    #[test]
    fn wave_modes_and_high_slots_are_not_evidence() {
        // BeatDrop has the modes up to 17 and 16 custom waves / shapes too
        let text = "[preset00]\nnWaveMode=16\nwavecode_9_enabled=1\nshapecode_12_enabled=1\n\
                    shapecode_12_sides=64\nwave_9_per_frame1=q1=1;\n";
        assert_eq!(family(text), DetectedFamily::ProjectM);
    }

    #[test]
    fn shader_prelude_names_are_evidence() {
        for (src, name) in [
            ("ret = get_fft(uv.x);", "get_fft"),
            ("ret = get_fft_hz(440);", "get_fft_hz"),
            ("ret = tex2D(sampler_fft, uv).x;", "sampler_fft"),
            ("ret = tex2D(sampler_fw_fft, uv).x;", "sampler_fw_fft"),
            ("ret = mouse.x;", "mouse"),
            ("ret = mouse_x;", "mouse_x"),
            ("ret = _c14.xyz;", "_c14"),
            ("ret = q33 + q64;", "q33"),
            ("ret = _qi.x;", "_qi"),
        ] {
            let text = comp(&["shader_body", "{", src, "}"]);
            let ev = evidence(&text);
            assert!(
                ev.contains(&Evidence::ShaderName {
                    stage: "comp",
                    name: name.into()
                }),
                "{src}: {ev:?}"
            );
        }
        // the warp shader is labelled as such
        let ev = evidence("warp_1=`shader_body{ ret = get_fft(0.5); }\n");
        assert_eq!(
            ev,
            [Evidence::ShaderName {
                stage: "warp",
                name: "get_fft".into()
            }]
        );
    }

    #[test]
    fn shader_false_positive_guards() {
        for src in [
            // comments
            "// get_fft(0.1) and q40",
            "/* sampler_fft */ ret = 0;",
            "ret = 0; /* mouse\nq33 */",
            // longer / different identifiers
            "ret = q330 + q65 + q32 + q033 + q3;",
            "ret = mouse_pos2 + mousex + xmouse + get_fft2 + my_get_fft;",
            "ret = get_fft_peak(0.5) + get_wave(0.5);",
            "ret = _c15.x + _c13.x + _qh.x + _qq.x;",
            "ret = sampler_fft2 + sampler_xx_fft;",
            // a member named like a prelude name
            "ret = foo.mouse + foo.q40;",
            // number literals
            "ret = 3q33 + 0x40;",
            // the preset declares the name itself
            "float2 mouse = float2(0.5, 0.5); ret = mouse.x;",
            "float get_fft(float x) { return x; } ret = get_fft(0.2);",
        ] {
            let text = comp(&["shader_body", "{", src, "}"]);
            assert_eq!(evidence(&text), [], "{src}");
        }
        // a `#define` of the name, used on the next line
        let text = comp(&["#define q40 0.5", "shader_body", "{", "ret = q40;", "}"]);
        assert_eq!(evidence(&text), []);
    }

    #[test]
    fn expression_variables_q33_to_q64() {
        let ev = evidence("[preset00]\nper_frame_1=q33 = 1; q64 = q33 + Q40;\n");
        assert_eq!(
            ev,
            [
                Evidence::ExpressionVariable {
                    part: "per_frame".into(),
                    name: "q33".into()
                },
                Evidence::ExpressionVariable {
                    part: "per_frame".into(),
                    name: "q64".into()
                },
                Evidence::ExpressionVariable {
                    part: "per_frame".into(),
                    name: "Q40".into()
                },
            ]
        );
        let ev = evidence("[preset00]\nshape_2_per_frame1=q50 = 1;\nwave_1_per_point1=y = q60;\n");
        assert_eq!(ev.len(), 2);
        assert!(
            matches!(&ev[0], Evidence::ExpressionVariable { part, .. } if part == "wave 1 per_point")
        );
        assert!(
            matches!(&ev[1], Evidence::ExpressionVariable { part, .. } if part == "shape 2 per_frame")
        );
    }

    #[test]
    fn expression_false_positive_guards() {
        for code in [
            "q32 = q1 + q31;",
            "q330 = 1; q65 = q330 + q3;",
            "q033 = 1;",
            "// q40 = 1;",
            "x = 1; \\\\ q50 here",
            "/* q40 */ x = 2;",
            "x = foo.q40;",
            "my_q40 = 1; q40x = 2; xq40 = 3;",
            "x = 1e33; y = 0q33;",
        ] {
            let text = format!("[preset00]\nper_frame_1={code}\n");
            assert_eq!(evidence(&text), [], "{code}");
        }
    }

    #[test]
    fn t9_to_t16_count_only_in_wave_and_shape_code() {
        let ev = evidence("[preset00]\nwave_2_per_point1=x = t9 + t16;\nshape_1_init1=t12 = 1;\n");
        let names: Vec<_> = ev
            .iter()
            .map(|e| match e {
                Evidence::ExpressionVariable { name, .. } => name.as_str(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(names, ["t9", "t16", "t12"]);
        for text in [
            // an ordinary variable of the per-frame / per-pixel code
            "per_frame_1=t9 = 1;\nper_pixel_1=zoom = t16;\n",
            // MilkDrop 2's own slots and names that merely look alike
            "wave_2_per_point1=x = t1 + t8 + t17 + t90 + t09 + t9x + xt9;\n",
        ] {
            assert_eq!(evidence(&format!("[preset00]\n{text}")), []);
        }
    }

    #[test]
    fn display_names_the_evidence() {
        assert_eq!(Evidence::SpriteBlock.to_string(), "sprite block");
        assert_eq!(
            Evidence::ShaderName {
                stage: "comp",
                name: "get_fft".into()
            }
            .to_string(),
            "comp shader uses get_fft"
        );
    }
}
