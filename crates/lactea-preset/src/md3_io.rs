//! MilkDrop 3 file output and single-slot files: the `.wave` / `.shape` formats and the MD3
//! `.milk` writer (MD3 3.37, docs/md3.md "`.wave` / `.shape` files and the MD3 `.milk` writer").
//!
//! MD3 prints floats with C `printf` (`%.3f` / `%.5f`) and ints with `%d`; a float is promoted
//! to `double` first, so formatting `f64::from(x)` with Rust's `{:.N}` (exact binary value,
//! round-half-even like glibc) gives the same digits.  Re-reading a written file therefore
//! reproduces values only up to that printed precision; code blocks are exact.

use crate::scan::Lookup;
use crate::{
    CustomShape, CustomWave, MAX_CUSTOM_SHAPES, MAX_CUSTOM_WAVES, ParseError, Preset, write_code,
};
use std::fmt::Write as _;

/// The comp/warp text MD3 writes in place of a closed shader contains this marker; only then is the
/// matching `MD31` / `MD32` handle written back.
const LOCKED_SHADER_MARKER: &str = "This shader";

fn check_input(bytes: &[u8]) -> Result<(), ParseError> {
    if bytes.is_empty() {
        return Err(ParseError::Empty);
    }
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return Err(ParseError::Utf16);
    }
    Ok(())
}

/// Import a `.wave` file into custom-wave slot `slot` of `preset`.
///
/// The file's own index is always 0 (`wavecode_0_*`, `wave_0_*`).  Like MD3 the slot is **not**
/// reset first: keys missing from the file keep the slot's previous values, while the three code
/// blocks are cleared and re-read (empty when the file has none).  The file is read with the
/// MilkDrop 2 line rules (first `=`/space separator, first duplicate wins, lenient numbers).
///
/// Errors: [`ParseError::SlotOutOfRange`] for `slot >= MAX_CUSTOM_WAVES`, [`ParseError::Empty`] /
/// [`ParseError::Utf16`] like [`Preset::parse`].  On error the preset is unchanged.
pub fn import_wave_file(preset: &mut Preset, slot: usize, bytes: &[u8]) -> Result<(), ParseError> {
    if slot >= MAX_CUSTOM_WAVES {
        return Err(ParseError::SlotOutOfRange {
            slot,
            count: MAX_CUSTOM_WAVES,
        });
    }
    check_input(bytes)?;
    let mut lk = Lookup::new(bytes);
    preset.waves[slot].import(&mut lk, 0);
    Ok(())
}

/// Import a `.shape` file into custom-shape slot `slot` of `preset`; same rules as
/// [`import_wave_file`] (keys `shapecode_0_*`, `shape_0_*`).
///
/// The projectM-only `image` key is not read (MD3 has no such key); the slot keeps its image.
pub fn import_shape_file(preset: &mut Preset, slot: usize, bytes: &[u8]) -> Result<(), ParseError> {
    if slot >= MAX_CUSTOM_SHAPES {
        return Err(ParseError::SlotOutOfRange {
            slot,
            count: MAX_CUSTOM_SHAPES,
        });
    }
    check_input(bytes)?;
    let mut lk = Lookup::new(bytes);
    preset.shapes[slot].import(&mut lk, 0);
    Ok(())
}

/// The text of a `.wave` file for custom-wave slot `slot` (index 0 lines only, no header), or
/// `None` when `slot >= MAX_CUSTOM_WAVES`.  Floats are printed with `%.5f` / `%.3f` like MD3.
pub fn export_wave_file(preset: &Preset, slot: usize) -> Option<String> {
    preset.waves.get(slot).map(CustomWave::to_file_text)
}

/// The text of a `.shape` file for custom-shape slot `slot`, or `None` when out of range.
pub fn export_shape_file(preset: &Preset, slot: usize) -> Option<String> {
    preset.shapes.get(slot).map(CustomShape::to_file_text)
}

fn f3(out: &mut String, key: &str, v: f32) {
    let _ = writeln!(out, "{key}={:.3}", f64::from(v));
}
fn f5(out: &mut String, key: &str, v: f32) {
    let _ = writeln!(out, "{key}={:.5}", f64::from(v));
}
fn d(out: &mut String, key: &str, v: i32) {
    let _ = writeln!(out, "{key}={v}");
}

impl CustomWave {
    /// MD3's wave block (`FUN_140089118`) with index `i`: `%d` / `%.5f` / `%.3f` fields in struct
    /// order, then the three code blocks (no backtick).
    fn write_md3(&self, i: usize, out: &mut String) {
        let k = |s: &str| format!("wavecode_{i}_{s}");
        d(out, &k("enabled"), self.enabled);
        d(out, &k("samples"), self.samples);
        d(out, &k("sep"), self.sep);
        d(out, &k("bSpectrum"), self.b_spectrum);
        d(out, &k("bUseDots"), self.b_use_dots);
        d(out, &k("bDrawThick"), self.b_draw_thick);
        d(out, &k("bAdditive"), self.b_additive);
        f5(out, &k("scaling"), self.scaling);
        f5(out, &k("smoothing"), self.smoothing);
        f3(out, &k("r"), self.r);
        f3(out, &k("g"), self.g);
        f3(out, &k("b"), self.b);
        f3(out, &k("a"), self.a);
        write_code(out, &format!("wave_{i}_init"), &self.init, false);
        write_code(out, &format!("wave_{i}_per_frame"), &self.per_frame, false);
        write_code(out, &format!("wave_{i}_per_point"), &self.per_point, false);
    }

    /// The `.wave` file text of this wave: its MD3 block with index 0.  Values are truncated to the
    /// printed precision (`%.5f` scaling/smoothing, `%.3f` colours); code is exact.
    pub fn to_file_text(&self) -> String {
        let mut out = String::new();
        self.write_md3(0, &mut out);
        out
    }
}

impl CustomShape {
    /// MD3's shape block (`FUN_140087620`) with index `i`.  The projectM-only `image` key is not
    /// written.
    fn write_md3(&self, i: usize, out: &mut String) {
        let k = |s: &str| format!("shapecode_{i}_{s}");
        d(out, &k("enabled"), self.enabled);
        d(out, &k("sides"), self.sides);
        d(out, &k("additive"), self.additive);
        d(out, &k("thickOutline"), self.thick_outline);
        d(out, &k("textured"), self.textured);
        d(out, &k("num_inst"), self.instances);
        f3(out, &k("x"), self.x);
        f3(out, &k("y"), self.y);
        f5(out, &k("rad"), self.rad);
        f5(out, &k("ang"), self.ang);
        f5(out, &k("tex_ang"), self.tex_ang);
        f5(out, &k("tex_zoom"), self.tex_zoom);
        for (name, v) in [
            ("r", self.r),
            ("g", self.g),
            ("b", self.b),
            ("a", self.a),
            ("r2", self.r2),
            ("g2", self.g2),
            ("b2", self.b2),
            ("a2", self.a2),
            ("border_r", self.border_r),
            ("border_g", self.border_g),
            ("border_b", self.border_b),
            ("border_a", self.border_a),
        ] {
            f3(out, &k(name), v);
        }
        write_code(out, &format!("shape_{i}_init"), &self.init, false);
        write_code(out, &format!("shape_{i}_per_frame"), &self.per_frame, false);
    }

    /// The `.shape` file text of this shape: its MD3 block with index 0 (`%.3f` / `%.5f` precision
    /// as MD3; code exact).
    pub fn to_file_text(&self) -> String {
        let mut out = String::new();
        self.write_md3(0, &mut out);
        out
    }
}

impl Preset {
    /// Whether a sprite block is attached to the preset.
    ///
    /// Sprites are not parsed by this crate: a sprite shows up only as unconsumed lines in
    /// [`Preset::extra`], so this is a stub that reports `true` when an `extra` key starts with
    /// `sprite` (ASCII case-insensitive).  Whoever parses sprites should replace it.
    pub fn has_sprite(&self) -> bool {
        self.extra
            .iter()
            .any(|(k, _)| k.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("sprite")))
    }

    /// The `MD31` / `MD32` handle for `key` when the matching shader text is a locked-shader stub.
    fn locked_handle(&self, key: &str, shader: Option<&String>) -> Option<u64> {
        if !shader?.contains(LOCKED_SHADER_MARKER) {
            return None;
        }
        self.extra
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.trim().parse().ok())
    }

    /// Serialise like MilkDrop 3 3.37 `Export` (docs/md3.md): no banner, `\n` line ends, `printf`
    /// precision (`%.3f` / `%.5f`), custom waves/shapes **compacted** to the slots with
    /// `enabled == 1` (numbered from 0 in slot order; `enabled == 2` is not written), shader text
    /// only for ps >= 2 and always backticked.
    ///
    /// Lines in order: `MD31=` / `MD32=` (only when the preset carries that handle in
    /// [`Preset::extra`] and the shader text is a locked-shader stub), the version lines (when
    /// [`Preset::max_ps_version`] > 0: `MILKDROP_PRESET_VERSION=300` if [`Preset::has_sprite`]
    /// else `201`, then `PSVERSION*`), `[preset00]`, the base values, waves, shapes, code blocks.
    ///
    /// MD3 does not round-trip unknown keys: the rest of [`Preset::extra`] is not written, and
    /// neither are the projectM-only shape `image` or a sprite section.  Use [`Preset::to_milk`]
    /// for a lossless library round trip.
    pub fn to_milk_md3(&self) -> String {
        let mut out = String::new();
        if let Some(h) = self.locked_handle("MD31", self.warp_shader.as_ref()) {
            let _ = writeln!(out, "MD31={h}");
        }
        if let Some(h) = self.locked_handle("MD32", self.comp_shader.as_ref()) {
            let _ = writeln!(out, "MD32={h}");
        }
        let max = self.max_ps_version();
        if max > 0 {
            let v = if self.has_sprite() { 300 } else { 201 };
            let _ = writeln!(out, "MILKDROP_PRESET_VERSION={v}");
            let _ = writeln!(out, "PSVERSION={max}");
            let _ = writeln!(out, "PSVERSION_WARP={}", self.warp_ps_version);
            let _ = writeln!(out, "PSVERSION_COMP={}", self.comp_ps_version);
        }
        out.push_str("[preset00]\n");
        self.write_base_md3(&mut out);
        for (n, w) in self.waves.iter().filter(|w| w.enabled == 1).enumerate() {
            w.write_md3(n, &mut out);
        }
        for (n, s) in self.shapes.iter().filter(|s| s.enabled == 1).enumerate() {
            s.write_md3(n, &mut out);
        }
        write_code(&mut out, "per_frame_init_", &self.per_frame_init, false);
        write_code(&mut out, "per_frame_", &self.per_frame, false);
        write_code(&mut out, "per_pixel_", &self.per_pixel, false);
        if self.warp_ps_version >= 2
            && let Some(w) = &self.warp_shader
        {
            write_code(&mut out, "warp_", w, true);
        }
        if self.comp_ps_version >= 2
            && let Some(c) = &self.comp_shader
        {
            write_code(&mut out, "comp_", c, true);
        }
        out
    }

    fn write_base_md3(&self, out: &mut String) {
        let b = &self.base;
        f3(out, "FFTAttack", b.fft_attack);
        f3(out, "FFTDecay", b.fft_decay);
        f3(out, "fRating", b.rating);
        f3(out, "fGammaAdj", b.gamma_adj);
        f3(out, "fDecay", b.decay);
        f3(out, "fVideoEchoZoom", b.video_echo_zoom);
        f3(out, "fVideoEchoAlpha", b.video_echo_alpha);
        d(out, "nVideoEchoOrientation", b.video_echo_orientation);
        d(out, "nWaveMode", b.wave_mode);
        for (key, v) in [
            ("bAdditiveWaves", b.additive_waves),
            ("bWaveDots", b.wave_dots),
            ("bWaveThick", b.wave_thick),
            ("bModWaveAlphaByVolume", b.mod_wave_alpha_by_volume),
            ("bMaximizeWaveColor", b.maximize_wave_color),
            ("bTexWrap", b.tex_wrap),
            ("bDarkenCenter", b.darken_center),
            ("bRedBlueStereo", b.red_blue_stereo),
            ("bBrighten", b.brighten),
            ("bDarken", b.darken),
            ("bSolarize", b.solarize),
            ("bInvert", b.invert),
        ] {
            d(out, key, i32::from(v));
        }
        f3(out, "fWaveAlpha", b.wave_alpha);
        f3(out, "fWaveScale", b.wave_scale);
        f3(out, "fWaveSmoothing", b.wave_smoothing);
        f3(out, "fWaveParam", b.wave_param);
        f3(out, "fModWaveAlphaStart", b.mod_wave_alpha_start);
        f3(out, "fModWaveAlphaEnd", b.mod_wave_alpha_end);
        f3(out, "fWarpAnimSpeed", b.warp_anim_speed);
        f3(out, "fWarpScale", b.warp_scale);
        f5(out, "fZoomExponent", b.zoom_exponent);
        f3(out, "fShader", b.shader);
        f5(out, "zoom", b.zoom);
        f5(out, "rot", b.rot);
        f3(out, "cx", b.cx);
        f3(out, "cy", b.cy);
        f5(out, "dx", b.dx);
        f5(out, "dy", b.dy);
        f5(out, "warp", b.warp);
        f5(out, "sx", b.sx);
        f5(out, "sy", b.sy);
        for (key, v) in [
            ("wave_r", b.wave_r),
            ("wave_g", b.wave_g),
            ("wave_b", b.wave_b),
            ("wave_x", b.wave_x),
            ("wave_y", b.wave_y),
            ("ob_size", b.ob_size),
            ("ob_r", b.ob_r),
            ("ob_g", b.ob_g),
            ("ob_b", b.ob_b),
            ("ob_a", b.ob_a),
            ("ib_size", b.ib_size),
            ("ib_r", b.ib_r),
            ("ib_g", b.ib_g),
            ("ib_b", b.ib_b),
            ("ib_a", b.ib_a),
            ("nMotionVectorsX", b.mv_x),
            ("nMotionVectorsY", b.mv_y),
            ("mv_dx", b.mv_dx),
            ("mv_dy", b.mv_dy),
            ("mv_l", b.mv_l),
            ("mv_r", b.mv_r),
            ("mv_g", b.mv_g),
            ("mv_b", b.mv_b),
            ("mv_a", b.mv_a),
            ("b1n", b.blur1_min),
            ("b2n", b.blur2_min),
            ("b3n", b.blur3_min),
            ("b1x", b.blur1_max),
            ("b2x", b.blur2_max),
            ("b3x", b.blur3_max),
            ("b1ed", b.blur1_edge_darken),
        ] {
            f3(out, key, v);
        }
    }
}
