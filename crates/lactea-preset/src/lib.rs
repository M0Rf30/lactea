//! MilkDrop 2 preset import (`.milk`), ported from `state.cpp` (`CState::Import`,
//! `CState::Default`, `ReadCode`, `CWave::Import`, `CShape::Import`).
//!
//! The crate does no I/O: bytes in, [`Preset`] out.  It parses *exactly* like MD2 does
//! (including its quirks) and can serialise a [`Preset`] back to
//! `.milk` text such that `Preset::parse(p.to_milk().as_bytes()) == p`.
//!
//! Code blocks (`per_frame_init`, `per_frame`, `per_pixel`, wave/shape code, warp/comp
//! shader text) are stored in MD2's joined form: each source line followed by
//! [`LINEFEED_CONTROL_CHAR`] (`'\u{1}'`).  Use [`code_lines`] and [`code_to_source`] to
//! consume them.

#![warn(missing_docs)]

mod cnum;
mod md3_io;
pub mod milk2;
mod scan;
mod sprite;

use scan::{Lookup, decode, sane_entries};
use std::collections::HashSet;
use std::fmt::Write as _;

pub use md3_io::{export_shape_file, export_wave_file, import_shape_file, import_wave_file};
pub use scan::LINEFEED_CONTROL_CHAR;
pub use sprite::PresetSprite;

/// `MAX_CUSTOM_WAVES` (md_defines.h)
pub const MAX_CUSTOM_WAVES: usize = 16;
/// `MAX_CUSTOM_SHAPES` (md_defines.h)
pub const MAX_CUSTOM_SHAPES: usize = 16;
/// `CUR_MILKDROP_PRESET_VERSION` (state.h)
pub const MILKDROP_PRESET_VERSION: i32 = 201;
/// Value assumed when the file has no `MILKDROP_PRESET_VERSION` key (MilkDrop 1 era).
pub const MILKDROP_PRESET_VERSION_DEFAULT: i32 = 100;
/// `NUM_Q_VAR`
pub const NUM_Q_VAR: usize = 32;
/// `NUM_T_VAR`
pub const NUM_T_VAR: usize = 8;

/// Pixel shader version enum (`plugin.h`, `MD2_PS_*`): the numbers stored in `PSVERSION*`.
pub mod psversion {
    /// No pixel shaders (MD1-era presets).
    pub const NONE: i32 = 0;
    /// `ps_2_0`
    pub const PS_2_0: i32 = 2;
    /// `ps_2_x`
    pub const PS_2_X: i32 = 3;
    /// `ps_3_0`
    pub const PS_3_0: i32 = 4;
    /// `ps_4_0`
    pub const PS_4_0: i32 = 5;
    /// Default when the key is missing in a version >= 200 file.
    pub const FILE_DEFAULT: i32 = 2;
}

/// Why a preset could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseError {
    /// Zero-length input (MD2 would silently yield all defaults).
    Empty,
    /// UTF-16 BOM: MD2 reads bytes and would see no keys at all.
    Utf16,
    /// A `.wave` / `.shape` import named a slot that does not exist.
    SlotOutOfRange {
        /// The requested slot.
        slot: usize,
        /// Number of slots of that kind (valid slots are `0..count`).
        count: usize,
    },
}

/// Which importer's reading rules [`Preset::parse_with`] follows.  The two disagree on
/// duplicate keys (MD2: cursor then first copy; projectM: first copy always, so a stray
/// `per_frame_8 ` blanks the real `per_frame_8=` below it), key case, over-long lines,
/// `~!@#$`, numeric parsing, boolean sign and a few missing-key defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Flavor {
    /// MilkDrop 2 `state.cpp`.
    #[default]
    Md2,
    /// projectM `PresetFileParser` + `PresetState::Initialize` (projectM reference behaviour).
    ProjectM,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Empty => f.write_str("empty preset"),
            ParseError::Utf16 => f.write_str("preset is UTF-16 (not supported by MilkDrop)"),
            ParseError::SlotOutOfRange { slot, count } => {
                write!(f, "custom wave/shape slot {slot} out of range (0..{count})")
            }
        }
    }
}
impl std::error::Error for ParseError {}

// ---------------------------------------------------------------------------------------
// field plumbing
// ---------------------------------------------------------------------------------------

trait Field: Sized {
    fn read(lk: &mut Lookup, key: &str, def: Self) -> Self;
    fn write(&self, out: &mut String);
}
impl Field for f32 {
    fn read(lk: &mut Lookup, key: &str, def: f32) -> f32 {
        lk.float(key, def)
    }
    fn write(&self, out: &mut String) {
        let _ = write!(out, "{self}");
    }
}
impl Field for i32 {
    fn read(lk: &mut Lookup, key: &str, def: i32) -> i32 {
        lk.int(key, def)
    }
    fn write(&self, out: &mut String) {
        let _ = write!(out, "{self}");
    }
}
impl Field for bool {
    fn read(lk: &mut Lookup, key: &str, def: bool) -> bool {
        lk.flag(key, def)
    }
    fn write(&self, out: &mut String) {
        out.push(if *self { '1' } else { '0' });
    }
}

macro_rules! pick {
    ($def:expr;) => {
        $def
    };
    ($def:expr; $imp:expr) => {
        $imp
    };
}

macro_rules! base_values {
    ($lk:ident;
     $( $sec:ident $parse_fn:ident { $( $(#[$m:meta])* $f:ident : $t:ty = $key:literal, $def:expr $(, $imp:expr)? ; )* } )*
    ) => {
        /// Scalar preset settings (`CState` members), named after the MD2 members.
        ///
        /// `Default` is `CState::Default()`.  Parsing uses the *import* defaults, which are
        /// equal except for the documented MD2 quirks (`wave_r/g/b/x/y`, `mv_a`).
        #[derive(Debug, Clone, PartialEq)]
        #[non_exhaustive]
        pub struct BaseValues {
            $( $( #[doc = concat!("File key `", $key, "`.")] $(#[$m])* pub $f: $t, )* )*
        }
        impl Default for BaseValues {
            fn default() -> Self {
                Self { $( $( $f: $def, )* )* }
            }
        }
        impl BaseValues {
            $(
                fn $parse_fn(&mut self, $lk: &mut Lookup) {
                    $( self.$f = { let imp: $t = pick!($def; $($imp)?); <$t as Field>::read($lk, $key, imp) }; )*
                }
            )*
            fn write_keys(&self, out: &mut String) {
                $( $(
                    out.push_str($key); out.push('=');
                    Field::write(&self.$f, out);
                    out.push('\n');
                )* )*
            }
            /// MD2 file keys that [`BaseValues`] consumes.
            pub const KEYS: &'static [&'static str] = &[ $( $( $key, )* )* ];
        }
    };
}

base_values! { lk;
    general parse_general {
        rating: f32 = "fRating", 3.0;
        decay: f32 = "fDecay", 0.98;
        /// BeatDrop extension.
        fft_attack: f32 = "FFTAttack", 0.5;
        /// BeatDrop extension.
        fft_decay: f32 = "FFTDecay", 0.7;
        gamma_adj: f32 = "fGammaAdj", 2.0;
        video_echo_zoom: f32 = "fVideoEchoZoom", 2.0;
        video_echo_alpha: f32 = "fVideoEchoAlpha", 0.0;
        /// 0-3, not range-checked at import.
        video_echo_orientation: i32 = "nVideoEchoOrientation", 0;
        red_blue_stereo: bool = "bRedBlueStereo", false;
        brighten: bool = "bBrighten", false;
        darken: bool = "bDarken", false;
        solarize: bool = "bSolarize", false;
        invert: bool = "bInvert", false;
        shader: f32 = "fShader", 0.0;
        blur1_min: f32 = "b1n", 0.0;
        blur2_min: f32 = "b2n", 0.0;
        blur3_min: f32 = "b3n", 0.0;
        blur1_max: f32 = "b1x", 1.0;
        blur2_max: f32 = "b2x", 1.0;
        blur3_max: f32 = "b3x", 1.0;
        blur1_edge_darken: f32 = "b1ed", 0.25;
    }
    wave parse_wave {
        wave_mode: i32 = "nWaveMode", 0;
        additive_waves: bool = "bAdditiveWaves", false;
        wave_dots: bool = "bWaveDots", false;
        wave_thick: bool = "bWaveThick", false;
        mod_wave_alpha_by_volume: bool = "bModWaveAlphaByVolume", false;
        maximize_wave_color: bool = "bMaximizeWaveColor", true;
        wave_alpha: f32 = "fWaveAlpha", 0.8;
        wave_scale: f32 = "fWaveScale", 1.0;
        wave_smoothing: f32 = "fWaveSmoothing", 0.75;
        wave_param: f32 = "fWaveParam", 0.0;
        mod_wave_alpha_start: f32 = "fModWaveAlphaStart", 0.75;
        mod_wave_alpha_end: f32 = "fModWaveAlphaEnd", 0.95;
        // MD2 QUIRK: the missing-key fallback of wave_r/g/b/x/y is `m_fRot.eval(-1)` (a
        // copy/paste slip in Import), i.e. 0.0 -- not the Default() values 1/1/1/.5/.5
        // (projectM's `PresetState` defaults are those).
        wave_r: f32 = "wave_r", 1.0, if lk.is_projectm() { 1.0 } else { 0.0 };
        wave_g: f32 = "wave_g", 1.0, if lk.is_projectm() { 1.0 } else { 0.0 };
        wave_b: f32 = "wave_b", 1.0, if lk.is_projectm() { 1.0 } else { 0.0 };
        wave_x: f32 = "wave_x", 0.5, if lk.is_projectm() { 0.5 } else { 0.0 };
        wave_y: f32 = "wave_y", 0.5, if lk.is_projectm() { 0.5 } else { 0.0 };
        /// Float in MD2.
        mv_x: f32 = "nMotionVectorsX", 12.0;
        mv_y: f32 = "nMotionVectorsY", 9.0;
        mv_dx: f32 = "mv_dx", 0.0;
        mv_dy: f32 = "mv_dy", 0.0;
        mv_l: f32 = "mv_l", 0.9;
        mv_r: f32 = "mv_r", 1.0;
        mv_g: f32 = "mv_g", 1.0;
        mv_b: f32 = "mv_b", 1.0;
        // MD2 QUIRK: fallback is derived from the legacy `bMotionVectorsOn` key
        // (missing -> off -> 0.0), not from Default()'s 1.0.
        mv_a: f32 = "mv_a", 1.0, if lk.flag("bMotionVectorsOn", false) { 1.0 } else { 0.0 };
    }
    motion parse_motion {
        zoom: f32 = "zoom", 1.0;
        rot: f32 = "rot", 0.0;
        cx: f32 = "cx", 0.5;
        cy: f32 = "cy", 0.5;
        dx: f32 = "dx", 0.0;
        dy: f32 = "dy", 0.0;
        warp: f32 = "warp", 1.0;
        sx: f32 = "sx", 1.0;
        sy: f32 = "sy", 1.0;
        tex_wrap: bool = "bTexWrap", true;
        darken_center: bool = "bDarkenCenter", false;
        warp_anim_speed: f32 = "fWarpAnimSpeed", 1.0;
        warp_scale: f32 = "fWarpScale", 1.0;
        zoom_exponent: f32 = "fZoomExponent", 1.0;
        ob_size: f32 = "ob_size", 0.01;
        ob_r: f32 = "ob_r", 0.0;
        ob_g: f32 = "ob_g", 0.0;
        ob_b: f32 = "ob_b", 0.0;
        ob_a: f32 = "ob_a", 0.0;
        ib_size: f32 = "ib_size", 0.01;
        ib_r: f32 = "ib_r", 0.25;
        ib_g: f32 = "ib_g", 0.25;
        ib_b: f32 = "ib_b", 0.25;
        ib_a: f32 = "ib_a", 0.0;
    }
}

// ---------------------------------------------------------------------------------------
// custom waves / shapes
// ---------------------------------------------------------------------------------------

/// A custom wave (`CWave`).  Keys: `wavecode_<i>_<field>`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct CustomWave {
    /// `enabled`
    pub enabled: i32,
    /// `samples`
    pub samples: i32,
    /// `sep`
    pub sep: i32,
    /// `bSpectrum`
    pub b_spectrum: i32,
    /// `bUseDots`
    pub b_use_dots: i32,
    /// `bDrawThick`
    pub b_draw_thick: i32,
    /// `bAdditive`
    pub b_additive: i32,
    /// `scaling`
    pub scaling: f32,
    /// `smoothing`
    pub smoothing: f32,
    /// `r`
    pub r: f32,
    /// `g`
    pub g: f32,
    /// `b`
    pub b: f32,
    /// `a`
    pub a: f32,
    /// `wave_<i>_init<n>` joined (MD2 form).
    pub init: String,
    /// `wave_<i>_per_frame<n>`
    pub per_frame: String,
    /// `wave_<i>_per_point<n>`
    pub per_point: String,
}

impl Default for CustomWave {
    fn default() -> Self {
        CustomWave {
            enabled: 0,
            samples: 512,
            sep: 0,
            b_spectrum: 0,
            b_use_dots: 0,
            b_draw_thick: 0,
            b_additive: 0,
            scaling: 1.0,
            smoothing: 0.5,
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 1.0,
            init: String::new(),
            per_frame: String::new(),
            per_point: String::new(),
        }
    }
}

/// A custom shape (`CShape`).  Keys: `shapecode_<i>_<field>`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct CustomShape {
    /// `enabled`
    pub enabled: i32,
    /// `sides`
    pub sides: i32,
    /// `additive`
    pub additive: i32,
    /// `thickOutline`
    pub thick_outline: i32,
    /// `textured`
    pub textured: i32,
    /// `num_inst`
    pub instances: i32,
    /// `x`
    pub x: f32,
    /// `y`
    pub y: f32,
    /// `rad`
    pub rad: f32,
    /// `ang`
    pub ang: f32,
    /// `tex_ang`
    pub tex_ang: f32,
    /// `tex_zoom`
    pub tex_zoom: f32,
    /// `r` (fill colour, inner)
    pub r: f32,
    /// `g`
    pub g: f32,
    /// `b`
    pub b: f32,
    /// `a`
    pub a: f32,
    /// `r2` (fill colour, outer)
    pub r2: f32,
    /// `g2`
    pub g2: f32,
    /// `b2`
    pub b2: f32,
    /// `a2`
    pub a2: f32,
    /// `border_r`
    pub border_r: f32,
    /// `border_g`
    pub border_g: f32,
    /// `border_b`
    pub border_b: f32,
    /// `border_a`
    pub border_a: f32,
    /// `shapecode_<i>_image` (projectM addition, [`Flavor::ProjectM`] only): name of the user
    /// texture a *textured* shape samples instead of the main image (empty = main image).
    /// A non-empty value is consumed (not kept in [`Preset::extra`]); an empty
    /// `shapecode_<i>_image=` line is not, so it stays in `extra` and is re-emitted verbatim
    /// by [`Preset::to_milk`].  [`Flavor::Md2`] never reads this key.
    pub image: String,
    /// `shape_<i>_init<n>`
    pub init: String,
    /// `shape_<i>_per_frame<n>`
    pub per_frame: String,
}

impl Default for CustomShape {
    fn default() -> Self {
        CustomShape {
            enabled: 0,
            sides: 4,
            additive: 0,
            thick_outline: 0,
            textured: 0,
            instances: 1,
            x: 0.5,
            y: 0.5,
            rad: 0.1,
            ang: 0.0,
            tex_ang: 0.0,
            tex_zoom: 1.0,
            r: 1.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
            r2: 0.0,
            g2: 1.0,
            b2: 0.0,
            a2: 0.0,
            border_r: 1.0,
            border_g: 1.0,
            border_b: 1.0,
            border_a: 0.1,
            image: String::new(),
            init: String::new(),
            per_frame: String::new(),
        }
    }
}

impl CustomWave {
    /// `CWave::Import`
    fn import(&mut self, lk: &mut Lookup, i: usize) {
        let k = |s: &str| format!("wavecode_{i}_{s}");
        self.enabled = lk.flag_int(&k("enabled"), self.enabled);
        self.samples = lk.int(&k("samples"), self.samples);
        self.sep = lk.int(&k("sep"), self.sep);
        self.b_spectrum = lk.flag_int(&k("bSpectrum"), self.b_spectrum);
        self.b_use_dots = lk.flag_int(&k("bUseDots"), self.b_use_dots);
        self.b_draw_thick = lk.flag_int(&k("bDrawThick"), self.b_draw_thick);
        self.b_additive = lk.flag_int(&k("bAdditive"), self.b_additive);
        self.scaling = lk.float(&k("scaling"), self.scaling);
        self.smoothing = lk.float(&k("smoothing"), self.smoothing);
        self.r = lk.float(&k("r"), self.r);
        self.g = lk.float(&k("g"), self.g);
        self.b = lk.float(&k("b"), self.b);
        self.a = lk.float(&k("a"), self.a);
        self.init = decode(&lk.read_code(&format!("wave_{i}_init")));
        self.per_frame = decode(&lk.read_code(&format!("wave_{i}_per_frame")));
        self.per_point = decode(&lk.read_code(&format!("wave_{i}_per_point")));
    }

    fn write(&self, i: usize, out: &mut String) {
        let _ = writeln!(out, "wavecode_{i}_enabled={}", self.enabled);
        let _ = writeln!(out, "wavecode_{i}_samples={}", self.samples);
        let _ = writeln!(out, "wavecode_{i}_sep={}", self.sep);
        let _ = writeln!(out, "wavecode_{i}_bSpectrum={}", self.b_spectrum);
        let _ = writeln!(out, "wavecode_{i}_bUseDots={}", self.b_use_dots);
        let _ = writeln!(out, "wavecode_{i}_bDrawThick={}", self.b_draw_thick);
        let _ = writeln!(out, "wavecode_{i}_bAdditive={}", self.b_additive);
        let _ = writeln!(out, "wavecode_{i}_scaling={}", self.scaling);
        let _ = writeln!(out, "wavecode_{i}_smoothing={}", self.smoothing);
        let _ = writeln!(out, "wavecode_{i}_r={}", self.r);
        let _ = writeln!(out, "wavecode_{i}_g={}", self.g);
        let _ = writeln!(out, "wavecode_{i}_b={}", self.b);
        let _ = writeln!(out, "wavecode_{i}_a={}", self.a);
        write_code(out, &format!("wave_{i}_init"), &self.init, false);
        write_code(out, &format!("wave_{i}_per_frame"), &self.per_frame, false);
        write_code(out, &format!("wave_{i}_per_point"), &self.per_point, false);
    }
}

impl CustomShape {
    /// `CShape::Import`
    fn import(&mut self, lk: &mut Lookup, i: usize) {
        let k = |s: &str| format!("shapecode_{i}_{s}");
        self.enabled = lk.flag_int(&k("enabled"), self.enabled);
        self.sides = lk.int(&k("sides"), self.sides);
        self.additive = lk.flag_int(&k("additive"), self.additive);
        self.thick_outline = lk.flag_int(&k("thickOutline"), self.thick_outline);
        self.textured = lk.flag_int(&k("textured"), self.textured);
        self.instances = lk.int(&k("num_inst"), self.instances);
        self.x = lk.float(&k("x"), self.x);
        self.y = lk.float(&k("y"), self.y);
        self.rad = lk.float(&k("rad"), self.rad);
        self.ang = lk.float(&k("ang"), self.ang);
        self.tex_ang = lk.float(&k("tex_ang"), self.tex_ang);
        self.tex_zoom = lk.float(&k("tex_zoom"), self.tex_zoom);
        self.r = lk.float(&k("r"), self.r);
        self.g = lk.float(&k("g"), self.g);
        self.b = lk.float(&k("b"), self.b);
        self.a = lk.float(&k("a"), self.a);
        self.r2 = lk.float(&k("r2"), self.r2);
        self.g2 = lk.float(&k("g2"), self.g2);
        self.b2 = lk.float(&k("b2"), self.b2);
        self.a2 = lk.float(&k("a2"), self.a2);
        self.border_r = lk.float(&k("border_r"), self.border_r);
        self.border_g = lk.float(&k("border_g"), self.border_g);
        self.border_b = lk.float(&k("border_b"), self.border_b);
        self.border_a = lk.float(&k("border_a"), self.border_a);
        if lk.is_projectm() {
            // projectM addition (`CustomShape::m_image`); MD2 has no such key, it stays in `extra` there
            self.image = decode(&lk.text(&k("image")));
        }
        self.init = decode(&lk.read_code(&format!("shape_{i}_init")));
        self.per_frame = decode(&lk.read_code(&format!("shape_{i}_per_frame")));
    }

    fn write(&self, i: usize, out: &mut String) {
        let mut f = |name: &str, v: String| {
            let _ = writeln!(out, "shapecode_{i}_{name}={v}");
        };
        f("enabled", self.enabled.to_string());
        f("sides", self.sides.to_string());
        f("additive", self.additive.to_string());
        f("thickOutline", self.thick_outline.to_string());
        f("textured", self.textured.to_string());
        f("num_inst", self.instances.to_string());
        f("x", self.x.to_string());
        f("y", self.y.to_string());
        f("rad", self.rad.to_string());
        f("ang", self.ang.to_string());
        f("tex_ang", self.tex_ang.to_string());
        f("tex_zoom", self.tex_zoom.to_string());
        f("r", self.r.to_string());
        f("g", self.g.to_string());
        f("b", self.b.to_string());
        f("a", self.a.to_string());
        f("r2", self.r2.to_string());
        f("g2", self.g2.to_string());
        f("b2", self.b2.to_string());
        f("a2", self.a2.to_string());
        f("border_r", self.border_r.to_string());
        f("border_g", self.border_g.to_string());
        f("border_b", self.border_b.to_string());
        f("border_a", self.border_a.to_string());
        if !self.image.is_empty() {
            let _ = writeln!(out, "shapecode_{i}_image={}", self.image);
        }
        write_code(out, &format!("shape_{i}_init"), &self.init, false);
        write_code(out, &format!("shape_{i}_per_frame"), &self.per_frame, false);
    }
}

// ---------------------------------------------------------------------------------------
// code helpers
// ---------------------------------------------------------------------------------------

/// Split an MD2-joined code block into its source lines.  Every line is terminated by
/// [`LINEFEED_CONTROL_CHAR`]; a terminator at the very end does not start a further empty
/// line, an unterminated tail still counts as a line, and an empty block has no lines
/// (`"a\u{1}\u{1}"` -> `["a", ""]`).  This is the line set `WriteCode` emits.
pub fn code_lines(code: &str) -> impl Iterator<Item = &str> {
    let mut rest = code;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        match rest.find(LINEFEED_CONTROL_CHAR) {
            Some(p) => {
                let l = &rest[..p];
                rest = &rest[p + 1..];
                Some(l)
            }
            None => {
                let l = rest;
                rest = "";
                Some(l)
            }
        }
    })
}

/// Joined block -> ordinary source text with `\n` line breaks (what projectM's parser sees).
pub fn code_to_source(code: &str) -> String {
    code.replace(LINEFEED_CONTROL_CHAR, "\n")
}

/// `CState::StripLinefeedCharsAndComments`, the way MilkDrop 2 turns a joined block into the text its
/// expression compiler sees: [`LINEFEED_CONTROL_CHAR`] is deleted (no replacement despite the MD2 comment, so a
/// statement split over two lines is glued together) and `//` / `\\` comments are dropped up to the next
/// separator. Compare [`code_to_source`], which projectM's parser needs.
pub fn strip_linefeeds_and_comments(code: &str) -> String {
    let b = code.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut comment = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let n = b.get(i + 1).copied().unwrap_or(0);
        if comment {
            if c == 1 {
                comment = false;
            }
        } else if (c == b'\\' && n == b'\\') || (c == b'/' && n == b'/') {
            comment = true;
        } else if c != 1 {
            out.push(c);
        }
        i += 1;
    }
    // only ASCII bytes (comments and 0x01 terminators) are removed, so UTF-8 stays valid
    String::from_utf8(out).expect("removed spans are ASCII-delimited")
}

fn write_code(out: &mut String, prefix: &str, code: &str, always_backtick: bool) {
    for (n, line) in code_lines(code).enumerate() {
        let tick = always_backtick || line.starts_with('`');
        let _ = writeln!(
            out,
            "{prefix}{}={}{line}",
            n + 1,
            if tick { "`" } else { "" }
        );
    }
}

// ---------------------------------------------------------------------------------------
// Preset
// ---------------------------------------------------------------------------------------

/// A parsed preset.
///
/// Non-exhaustive: construct it with [`Preset::default`], [`Preset::parse`] or
/// [`Preset::parse_with`] and assign fields.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Preset {
    /// `MILKDROP_PRESET_VERSION` (100 if the key is missing).
    pub milkdrop_preset_version: i32,
    /// `m_nWarpPSVersion`: 0 for version < 200, `PSVERSION` for 200, `PSVERSION_WARP`
    /// (default 2) for >= 201.
    pub warp_ps_version: i32,
    /// `m_nCompPSVersion` (same rules, `PSVERSION_COMP`).
    pub comp_ps_version: i32,
    /// Scalar settings (everything that is not a custom wave / shape or code).
    pub base: BaseValues,
    /// Always `MAX_CUSTOM_WAVES` (16) entries.
    pub waves: Vec<CustomWave>,
    /// Always `MAX_CUSTOM_SHAPES` (16) entries.
    pub shapes: Vec<CustomShape>,
    /// `per_frame_init_<n>` joined.
    pub per_frame_init: String,
    /// `per_frame_<n>` joined.
    pub per_frame: String,
    /// `per_pixel_<n>` joined.
    pub per_pixel: String,
    /// `warp_<n>` text; `None` when no `warp_1` key exists (MD2 then generates the default
    /// warp shader via `GenWarpPShaderText`, which is the HLSL slice's job).
    pub warp_shader: Option<String>,
    /// `comp_<n>` text; `None` -> default comp shader (`GenCompPShaderText`).
    pub comp_shader: Option<String>,
    /// MD3 sprite block (`SpriteName` .. `SpriteRepeatY`, `code_N`); `None` when the preset has no sprite name.
    /// MD2 and projectM ignore it.
    pub sprite: Option<PresetSprite>,
    /// Every other `key=value` line (file order, duplicates kept), decoded lossily.
    pub extra: Vec<(String, String)>,
}

impl Default for Preset {
    fn default() -> Self {
        Preset {
            milkdrop_preset_version: MILKDROP_PRESET_VERSION_DEFAULT,
            warp_ps_version: 0,
            comp_ps_version: 0,
            base: BaseValues::default(),
            waves: vec![CustomWave::default(); MAX_CUSTOM_WAVES],
            shapes: vec![CustomShape::default(); MAX_CUSTOM_SHAPES],
            per_frame_init: String::new(),
            per_frame: String::new(),
            per_pixel: String::new(),
            warp_shader: None,
            comp_shader: None,
            sprite: None,
            extra: Vec::new(),
        }
    }
}

impl Preset {
    /// `m_nMaxPSVersion`
    pub fn max_ps_version(&self) -> i32 {
        self.warp_ps_version.max(self.comp_ps_version)
    }
    /// `m_nMinPSVersion`
    pub fn min_ps_version(&self) -> i32 {
        self.warp_ps_version.min(self.comp_ps_version)
    }

    /// `CState::Import` with `ApplyFlags == STATE_ALL` on an in-memory file
    /// ([`Flavor::Md2`]).
    pub fn parse(bytes: &[u8]) -> Result<Preset, ParseError> {
        Self::parse_with(bytes, Flavor::Md2)
    }

    /// Parse like the given importer; see [`Flavor`].
    pub fn parse_with(bytes: &[u8], flavor: Flavor) -> Result<Preset, ParseError> {
        if bytes.is_empty() {
            return Err(ParseError::Empty);
        }
        if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
            return Err(ParseError::Utf16);
        }
        let mut lk = match flavor {
            Flavor::Md2 => Lookup::new(bytes),
            Flavor::ProjectM => Lookup::projectm(bytes),
        };
        let mut p = Preset::default();
        if flavor == Flavor::ProjectM {
            // `CustomShape::m_border_a{0.0f}` (MD2's `CShape::Default` has 0.1)
            for s in &mut p.shapes {
                s.border_a = 0.0;
            }
        }

        p.milkdrop_preset_version =
            lk.int("MILKDROP_PRESET_VERSION", MILKDROP_PRESET_VERSION_DEFAULT);
        let v = p.milkdrop_preset_version;
        let (warp_v, comp_v) = if v < 200 {
            (0, 0)
        } else if v == 200 {
            let x = lk.int("PSVERSION", psversion::FILE_DEFAULT);
            (x, x)
        } else {
            let w = lk.int("PSVERSION_WARP", psversion::FILE_DEFAULT);
            let c = lk.int("PSVERSION_COMP", psversion::FILE_DEFAULT);
            (w, c)
        };

        // STATE_GENERAL
        p.base.parse_general(&mut lk);
        // STATE_WAVE
        p.base.parse_wave(&mut lk);
        for i in 0..MAX_CUSTOM_WAVES {
            p.waves[i].import(&mut lk, i);
        }
        for i in 0..MAX_CUSTOM_SHAPES {
            p.shapes[i].import(&mut lk, i);
        }
        // STATE_MOTION
        p.base.parse_motion(&mut lk);
        p.per_frame_init = decode(&lk.read_code("per_frame_init_"));
        p.per_frame = decode(&lk.read_code("per_frame_"));
        p.per_pixel = decode(&lk.read_code("per_pixel_"));
        // STATE_WARP
        let w = decode(&lk.read_code("warp_"));
        p.warp_shader = if w.is_empty() { None } else { Some(w) };
        p.warp_ps_version = warp_v;
        // STATE_COMP
        let c = decode(&lk.read_code("comp_"));
        p.comp_shader = if c.is_empty() { None } else { Some(c) };
        // MD3 sprite block: read wherever it is, only meaningful to `Compat::MilkDrop3`
        p.sprite = PresetSprite::parse(&mut lk, bytes);
        p.comp_ps_version = comp_v;

        p.extra = p.collect_extra(bytes);
        Ok(p)
    }

    /// Keys MD2 looked at (or would have, were they present), as exact names.
    fn consumed_keys(&self) -> HashSet<String> {
        let mut s: HashSet<String> = HashSet::new();
        for k in BaseValues::KEYS {
            s.insert((*k).to_owned());
        }
        for k in [
            "MILKDROP_PRESET_VERSION",
            "PSVERSION",
            "PSVERSION_WARP",
            "PSVERSION_COMP",
            "bMotionVectorsOn",
        ] {
            s.insert(k.to_owned());
        }
        const WAVE: [&str; 13] = [
            "enabled",
            "samples",
            "sep",
            "bSpectrum",
            "bUseDots",
            "bDrawThick",
            "bAdditive",
            "scaling",
            "smoothing",
            "r",
            "g",
            "b",
            "a",
        ];
        const SHAPE: [&str; 24] = [
            "enabled",
            "sides",
            "additive",
            "thickOutline",
            "textured",
            "num_inst",
            "x",
            "y",
            "rad",
            "ang",
            "tex_ang",
            "tex_zoom",
            "r",
            "g",
            "b",
            "a",
            "r2",
            "g2",
            "b2",
            "a2",
            "border_r",
            "border_g",
            "border_b",
            "border_a",
        ];
        for i in 0..MAX_CUSTOM_WAVES {
            for k in WAVE {
                s.insert(format!("wavecode_{i}_{k}"));
            }
            for (pfx, code) in [
                ("init", &self.waves[i].init),
                ("per_frame", &self.waves[i].per_frame),
                ("per_point", &self.waves[i].per_point),
            ] {
                for n in 1..=code_lines(code).count() {
                    s.insert(format!("wave_{i}_{pfx}{n}"));
                }
            }
        }
        for i in 0..MAX_CUSTOM_SHAPES {
            for k in SHAPE {
                s.insert(format!("shapecode_{i}_{k}"));
            }
            if !self.shapes[i].image.is_empty() {
                s.insert(format!("shapecode_{i}_image"));
            }
            for (pfx, code) in [
                ("init", &self.shapes[i].init),
                ("per_frame", &self.shapes[i].per_frame),
            ] {
                for n in 1..=code_lines(code).count() {
                    s.insert(format!("shape_{i}_{pfx}{n}"));
                }
            }
        }
        for (pfx, code) in [
            ("per_frame_init_", Some(&self.per_frame_init)),
            ("per_frame_", Some(&self.per_frame)),
            ("per_pixel_", Some(&self.per_pixel)),
            ("warp_", self.warp_shader.as_ref()),
            ("comp_", self.comp_shader.as_ref()),
        ] {
            if let Some(code) = code {
                for n in 1..=code_lines(code).count() {
                    s.insert(format!("{pfx}{n}"));
                }
            }
        }
        if let Some(sp) = &self.sprite {
            s.extend(sp.consumed_keys());
        }
        s
    }

    fn collect_extra(&self, bytes: &[u8]) -> Vec<(String, String)> {
        let consumed = self.consumed_keys();
        sane_entries(bytes)
            .into_iter()
            .map(|(k, v)| (decode(&k), decode(&v)))
            .filter(|(k, _)| !consumed.contains(k))
            .collect()
    }

    /// Serialise to `.milk` text.  Floats are written with Rust's shortest round-trip
    /// formatting (not MD2's `%.3f`), so `parse(to_milk(p)) == p` holds for every preset
    /// produced by [`Preset::parse`].  Only customs differing from their defaults are
    /// written.  Shader text lines are written with a leading backtick like MD2's Export.
    pub fn to_milk(&self) -> String {
        let mut out = String::new();
        let v = self.milkdrop_preset_version;
        let _ = writeln!(out, "MILKDROP_PRESET_VERSION={v}");
        if v == 200 {
            let _ = writeln!(out, "PSVERSION={}", self.warp_ps_version);
        } else if v > 200 {
            let _ = writeln!(out, "PSVERSION={}", self.max_ps_version());
            let _ = writeln!(out, "PSVERSION_WARP={}", self.warp_ps_version);
            let _ = writeln!(out, "PSVERSION_COMP={}", self.comp_ps_version);
        }
        out.push_str("[preset00]\n");
        self.base.write_keys(&mut out);
        let dw = CustomWave::default();
        for (i, w) in self.waves.iter().enumerate() {
            if *w != dw {
                w.write(i, &mut out);
            }
        }
        let ds = CustomShape::default();
        for (i, s) in self.shapes.iter().enumerate() {
            if *s != ds {
                s.write(i, &mut out);
            }
        }
        write_code(&mut out, "per_frame_init_", &self.per_frame_init, false);
        write_code(&mut out, "per_frame_", &self.per_frame, false);
        write_code(&mut out, "per_pixel_", &self.per_pixel, false);
        if let Some(w) = &self.warp_shader {
            write_code(&mut out, "warp_", w, true);
        }
        if let Some(c) = &self.comp_shader {
            write_code(&mut out, "comp_", c, true);
        }
        if let Some(sp) = &self.sprite {
            sp.write(&mut out);
        }
        for (k, v) in &self.extra {
            let _ = writeln!(out, "{k}={v}");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_comments_like_md2() {
        assert_eq!(
            strip_linefeeds_and_comments("a=1; // c\u{1}b=2;\u{1}"),
            "a=1; b=2;"
        );
        assert_eq!(
            strip_linefeeds_and_comments("a=1;\\\\ c\u{1}b=2;\u{1}"),
            "a=1;b=2;"
        );
        assert_eq!(strip_linefeeds_and_comments("x=a/b;\u{1}"), "x=a/b;");
    }

    #[test]
    fn code_lines_terminator_rules() {
        let lines = |s| code_lines(s).collect::<Vec<_>>();
        assert_eq!(lines(""), Vec::<&str>::new());
        assert_eq!(lines("a\u{1}"), ["a"]);
        assert_eq!(lines("a\u{1}\u{1}"), ["a", ""]);
        assert_eq!(lines("a\u{1}b"), ["a", "b"]);
    }
}
