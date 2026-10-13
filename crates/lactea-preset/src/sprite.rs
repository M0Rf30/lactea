//! The sprite block of a MilkDrop 3 preset (`[SPRITE1_BEGIN]` .. `[SPRITE1_END]`).
//!
//! MD3 3.37 (docs/md3.md, sprites): one external image reference, a handful of placement numbers and
//! optional per-frame code. Nothing is embedded in the preset. The block header is never parsed: keys are
//! looked up like every other key of the file (section-agnostic, first occurrence wins), except
//! `SpriteName`, whose *last* occurrence wins.

use crate::scan::{Lookup, decode};
use crate::{code_lines, write_code};
use std::fmt::Write as _;

/// Scalar keys of the block (`SpriteName` and the `code_N` lines are handled separately).
const SCALAR_KEYS: [&str; 12] = [
    "SpriteColorKey",
    "SpriteLayer",
    "SpriteBlend",
    "SpriteAlpha",
    "SpriteBurn",
    "SpriteX",
    "SpriteY",
    "SpriteSX",
    "SpriteSY",
    "SpriteRot",
    "SpriteSpeed",
    "SpriteRepeatX",
];

/// `fgets` buffer of the `SpriteName` scan: a longer line is read in pieces of at most 255 bytes.
const NAME_SCAN_CHUNK: usize = 255;

/// The sprite of a preset (MD3 `SpriteName` .. `SpriteRepeatY` plus the block's `code_N` lines).
///
/// Absent keys take the defaults below (MD3 keeps stale values of the previously loaded preset; the
/// writer always emits every key, so the difference only shows in hand-written files).
#[derive(Debug, Clone, PartialEq)]
pub struct PresetSprite {
    /// `SpriteName`: image path relative to the MD3 data root, written with `\` (`sprites\x.png`).
    pub name: String,
    /// `SpriteColorKey`: 24-bit RGB (`%X`) that becomes transparent in static images (default 0, black).
    pub color_key: u32,
    /// `SpriteLayer`: 0..=4 are drawn, any other value is never drawn (default 0).
    pub layer: i32,
    /// `SpriteBlend`: 0..=10, anything else behaves as 0 (default 4).
    pub blend: i32,
    /// `SpriteAlpha`: multiplies the per-frame `a`, not clamped afterwards (default 1).
    pub alpha: f32,
    /// `SpriteBurn`: 0 never burns, 1 always, any other value follows the per-frame `burn` (default 1).
    pub burn: i32,
    /// `SpriteX`: additive offset of the per-frame `x`, in clip units (default 0).
    pub x: f32,
    /// `SpriteY` (default 0).
    pub y: f32,
    /// `SpriteSX`: additive offset of the per-frame `sx` (default -0.5).
    pub sx: f32,
    /// `SpriteSY` (default -0.5).
    pub sy: f32,
    /// `SpriteRot`: a rotation *mode*, not an angle: exactly `1.0` adds `time * speed` to the angle, exactly
    /// `2.0` subtracts it, anything else adds nothing (default 0).
    pub rot_mode: f32,
    /// `SpriteSpeed`: radians per second of the rotation mode (default 0).
    pub speed: f32,
    /// `SpriteRepeatX`: UV tiling multiplier (default 1).
    pub repeat_x: f32,
    /// `SpriteRepeatY` (default 1).
    pub repeat_y: f32,
    /// The block's `code_1`, `code_2`, .. lines in the joined form of [`crate::code_lines`].
    pub code: String,
}

impl Default for PresetSprite {
    fn default() -> Self {
        PresetSprite {
            name: String::new(),
            color_key: 0,
            layer: 0,
            blend: 4,
            alpha: 1.0,
            burn: 1,
            x: 0.0,
            y: 0.0,
            sx: -0.5,
            sy: -0.5,
            rot_mode: 0.0,
            speed: 0.0,
            repeat_x: 1.0,
            repeat_y: 1.0,
            code: String::new(),
        }
    }
}

/// `sscanf("%X")`: leading blanks, optional `0x` / `0X`, hex digits, anything after is ignored. `None` when no
/// digit is read.
fn scan_color_key(b: &[u8]) -> Option<u32> {
    let s = b.trim_ascii_start();
    let s = match s {
        [b'0', b'x' | b'X', rest @ ..] if rest.first().is_some_and(u8::is_ascii_hexdigit) => rest,
        _ => s,
    };
    let digits = s.iter().take_while(|c| c.is_ascii_hexdigit()).count();
    if digits == 0 {
        return None;
    }
    // more than 8 digits overflow the C `unsigned int`; keep the low 32 bits
    let v = s[..digits].iter().fold(0u64, |a, &c| {
        ((a << 4) | u64::from((c as char).to_digit(16).unwrap_or(0))) & 0xFFFF_FFFF
    });
    Some(v as u32)
}

/// Value of the last line that contains `SpriteName=`: the text after that key without trailing
/// whitespace. `None` when no line has the key.
fn scan_name(data: &[u8]) -> Option<Vec<u8>> {
    const KEY: &[u8] = b"SpriteName=";
    let mut found = None;
    for line in data.split(|&b| b == b'\n') {
        for piece in line.chunks(NAME_SCAN_CHUNK) {
            if let Some(at) = piece.windows(KEY.len()).position(|w| w == KEY) {
                found = Some(piece[at + KEY.len()..].trim_ascii_end().to_vec());
            }
        }
    }
    found
}

impl PresetSprite {
    /// Read the block from a preset's text. `None` unless a non-empty `SpriteName` is present (MD3 shows an
    /// error banner and draws nothing for an empty name).
    pub(crate) fn parse(lk: &mut Lookup, data: &[u8]) -> Option<PresetSprite> {
        let name = decode(&scan_name(data)?);
        if name.is_empty() {
            return None;
        }
        let d = PresetSprite::default();
        let color_key = scan_color_key(&lk.text("SpriteColorKey")).unwrap_or(d.color_key);
        let layer = lk.int("SpriteLayer", d.layer);
        let blend = lk.int("SpriteBlend", d.blend);
        let alpha = lk.float("SpriteAlpha", d.alpha);
        let burn = lk.int("SpriteBurn", d.burn);
        let x = lk.float("SpriteX", d.x);
        let y = lk.float("SpriteY", d.y);
        let sx = lk.float("SpriteSX", d.sx);
        let sy = lk.float("SpriteSY", d.sy);
        let rot_mode = lk.float("SpriteRot", d.rot_mode);
        let speed = lk.float("SpriteSpeed", d.speed);
        let repeat_x = lk.float("SpriteRepeatX", d.repeat_x);
        let repeat_y = lk.float("SpriteRepeatY", d.repeat_y);
        let code = decode(&lk.read_code("code_"));
        Some(PresetSprite {
            name,
            color_key,
            layer,
            blend,
            alpha,
            burn,
            x,
            y,
            sx,
            sy,
            rot_mode,
            speed,
            repeat_x,
            repeat_y,
            code,
        })
    }

    /// Every key this sprite was read from (they are not "extra" keys).
    pub(crate) fn consumed_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = SCALAR_KEYS.iter().map(|k| (*k).to_owned()).collect();
        keys.push("SpriteName".into());
        keys.push("SpriteRepeatY".into());
        keys.extend((1..=code_lines(&self.code).count()).map(|n| format!("code_{n}")));
        keys
    }

    /// The block as the MD3 writer lays it out (same key order, shortest round-trip floats).
    pub(crate) fn write(&self, out: &mut String) {
        out.push_str("[SPRITE1_BEGIN]\n");
        let _ = writeln!(out, "SpriteName={}", self.name);
        let _ = writeln!(out, "SpriteColorKey=0x{:X}", self.color_key);
        let _ = writeln!(out, "SpriteLayer={}", self.layer);
        let _ = writeln!(out, "SpriteBlend={}", self.blend);
        let _ = writeln!(out, "SpriteAlpha={}", self.alpha);
        let _ = writeln!(out, "SpriteBurn={}", self.burn);
        let _ = writeln!(out, "SpriteX={}", self.x);
        let _ = writeln!(out, "SpriteY={}", self.y);
        let _ = writeln!(out, "SpriteSX={}", self.sx);
        let _ = writeln!(out, "SpriteSY={}", self.sy);
        let _ = writeln!(out, "SpriteRot={}", self.rot_mode);
        let _ = writeln!(out, "SpriteSpeed={}", self.speed);
        let _ = writeln!(out, "SpriteRepeatX={}", self.repeat_x);
        let _ = writeln!(out, "SpriteRepeatY={}", self.repeat_y);
        write_code(out, "code_", &self.code, false);
        out.push_str("[SPRITE1_END]\n");
    }

    /// The block as MD3 3.37's `.milk` writer emits it (docs/md3.md, sprites): a blank line first, scalars with
    /// `printf` `%.6f` / `%d` / `0x%X`.
    pub(crate) fn write_md3(&self, out: &mut String) {
        out.push_str("\n[SPRITE1_BEGIN]\n");
        let _ = writeln!(out, "SpriteName={}", self.name);
        // `0x%X` without padding, except that the value 0 is written literally as `0x000000`
        if self.color_key == 0 {
            out.push_str("SpriteColorKey=0x000000\n");
        } else {
            let _ = writeln!(out, "SpriteColorKey=0x{:X}", self.color_key);
        }
        let _ = writeln!(out, "SpriteLayer={}", self.layer);
        let _ = writeln!(out, "SpriteBlend={}", self.blend);
        let _ = writeln!(out, "SpriteAlpha={:.6}", self.alpha);
        let _ = writeln!(out, "SpriteBurn={}", self.burn);
        let _ = writeln!(out, "SpriteX={:.6}", self.x);
        let _ = writeln!(out, "SpriteY={:.6}", self.y);
        let _ = writeln!(out, "SpriteSX={:.6}", self.sx);
        let _ = writeln!(out, "SpriteSY={:.6}", self.sy);
        let _ = writeln!(out, "SpriteRot={:.6}", self.rot_mode);
        let _ = writeln!(out, "SpriteSpeed={:.6}", self.speed);
        let _ = writeln!(out, "SpriteRepeatX={:.6}", self.repeat_x);
        let _ = writeln!(out, "SpriteRepeatY={:.6}", self.repeat_y);
        write_code(out, "code_", &self.code, false);
        out.push_str("[SPRITE1_END]\n");
    }
}

#[cfg(test)]
mod tests {
    use crate::{Flavor, Preset};

    /// The layout the MD3 writer produces.
    const BLOCK: &str = "\n[SPRITE1_BEGIN]\nSpriteName=sprites\\Jello1.png\nSpriteColorKey=0x000000\nSpriteLayer=0\nSpriteBlend=7\nSpriteAlpha=1.000000\nSpriteBurn=1\nSpriteX=0.000000\nSpriteY=0.000000\nSpriteSX=-0.800000\nSpriteSY=-0.800000\nSpriteRot=1.000000\nSpriteSpeed=1.500000\nSpriteRepeatX=1.000000\nSpriteRepeatY=1.000000\ncode_1=x=0.5+0.1*sin(time);\ncode_2=// a comment\ncode_3=a=0.8;\n[SPRITE1_END]\n";

    fn preset(extra: &str) -> Vec<u8> {
        format!("MILKDROP_PRESET_VERSION=300\n[preset00]\nzoom=1.01\n{extra}").into_bytes()
    }

    #[test]
    fn reads_the_md3_writer_layout() {
        for flavor in [Flavor::Md2, Flavor::ProjectM] {
            let p = Preset::parse_with(&preset(BLOCK), flavor).unwrap();
            let s = p.sprite.expect("sprite block");
            assert_eq!(s.name, "sprites\\Jello1.png");
            assert_eq!((s.color_key, s.layer, s.blend, s.burn), (0, 0, 7, 1));
            assert_eq!((s.alpha, s.x, s.y), (1.0, 0.0, 0.0));
            assert_eq!((s.sx, s.sy), (-0.8, -0.8));
            assert_eq!((s.rot_mode, s.speed), (1.0, 1.5));
            assert_eq!((s.repeat_x, s.repeat_y), (1.0, 1.0));
            assert_eq!(
                s.code,
                "x=0.5+0.1*sin(time);\u{1}// a comment\u{1}a=0.8;\u{1}"
            );
            // the sprite keys are not "extra" keys any more
            assert!(p.extra.iter().all(|(k, _)| !k.starts_with("Sprite")));
        }
    }

    #[test]
    fn missing_keys_take_the_defaults_and_hex_key_is_read() {
        let p = Preset::parse(&preset("SpriteName=a.png\nSpriteColorKey=0xFF00A0\n")).unwrap();
        let s = p.sprite.unwrap();
        assert_eq!(s.color_key, 0xFF00A0);
        assert_eq!((s.layer, s.blend, s.burn, s.alpha), (0, 4, 1, 1.0));
        assert_eq!((s.sx, s.sy, s.repeat_x, s.rot_mode), (-0.5, -0.5, 1.0, 0.0));
        assert_eq!(s.code, "");
        // `%X` also accepts a bare hex number
        let p = Preset::parse(&preset("SpriteName=a.png\nSpriteColorKey=1f\n")).unwrap();
        assert_eq!(p.sprite.unwrap().color_key, 0x1F);
    }

    #[test]
    fn name_is_the_last_occurrence_and_other_keys_the_first() {
        let text = preset(
            "SpriteName=first.png\nSpriteLayer=1\nSpriteLayer=3\nSpriteName=second.png  \r\n",
        );
        let s = Preset::parse(&text).unwrap().sprite.unwrap();
        assert_eq!(s.name, "second.png");
        assert_eq!(s.layer, 1);
    }

    #[test]
    fn name_is_read_after_its_own_key_not_the_first_equals() {
        let s = Preset::parse(&preset("foo=bar SpriteName=a.png\n"))
            .unwrap()
            .sprite
            .unwrap();
        assert_eq!(s.name, "a.png");
    }

    #[test]
    fn no_name_means_no_sprite_and_the_keys_stay_extra() {
        let p = Preset::parse(&preset("SpriteLayer=1\nSpriteName=\n")).unwrap();
        assert!(p.sprite.is_none());
        assert!(p.extra.iter().any(|(k, _)| k == "SpriteLayer"));
        assert!(Preset::parse(&preset("")).unwrap().sprite.is_none());
    }

    #[test]
    fn section_header_is_not_needed() {
        let text = preset("[SPRITE1]\nSpriteName=sprites\\a.png\nSpriteBlend=10\n");
        assert_eq!(Preset::parse(&text).unwrap().sprite.unwrap().blend, 10);
    }

    #[test]
    fn sprite_block_round_trips_through_to_milk() {
        for flavor in [Flavor::Md2, Flavor::ProjectM] {
            let p = Preset::parse_with(&preset(BLOCK), flavor).unwrap();
            let again = Preset::parse_with(p.to_milk().as_bytes(), flavor).unwrap();
            assert_eq!(p, again);
            assert!(p.to_milk().contains("[SPRITE1_BEGIN]"));
        }
    }
}
