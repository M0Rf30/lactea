//! The `imgNN` section of a MilkDrop sprite INI file, as projectM's `user_sprites.h` API takes it.

use crate::code_to_source;
use crate::scan::{Lookup, decode};

/// projectM's `PresetFileParser::maxFileSize`.
const MAX_BYTES: usize = 0x10_0000;

/// A parsed user sprite: the image name and the two code blocks (`\n`-joined source text).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct UserSprite {
    /// `img` (as written; the host lowercases and resolves it).
    pub img: String,
    /// `init_1`, `init_2`, ... : runs once at creation. Empty when absent.
    pub init: String,
    /// `code_1`, `code_2`, ... : runs every frame. Empty when absent.
    pub code: String,
}

/// Read the text of an `imgNN` section like projectM's `PresetFileParser`: `key<space or =>value` per line, keys
/// case-insensitive, first occurrence wins, header lines without a separator ignored. `None` where projectM refuses
/// the data: over 1 MiB, a NUL byte, or no key/value line at all.
pub fn parse_user_sprite(bytes: &[u8]) -> Option<UserSprite> {
    if bytes.len() > MAX_BYTES || bytes.contains(&0) {
        return None;
    }
    let has_entry = bytes.split(|&b| b == b'\r' || b == b'\n').any(|line| {
        line.iter()
            .position(|&b| b == b' ' || b == b'=')
            .is_some_and(|d| d > 0)
    });
    if !has_entry {
        return None;
    }
    let mut lk = Lookup::projectm(bytes);
    let source = |lk: &mut Lookup, prefix: &str| code_to_source(&decode(&lk.read_code(prefix)));
    Some(UserSprite {
        img: decode(&lk.text("img")),
        init: source(&mut lk, "init_"),
        code: source(&mut lk, "code_"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_keys_case_insensitively_and_joins_code_lines() {
        let s = parse_user_sprite(
            b"[img00]\r\nIMG=Logo.PNG\r\ninit_1=x=1;\r\ninit_2=y=2;\r\ncode_1=`a=3;\r\nimg=other\r\n",
        )
        .unwrap();
        assert_eq!(s.img, "Logo.PNG");
        assert_eq!(s.init, "x=1;\ny=2;\n");
        assert_eq!(s.code, "a=3;\n");
    }

    #[test]
    fn refuses_what_projectm_refuses() {
        assert!(parse_user_sprite(b"").is_none());
        assert!(parse_user_sprite(b"[img00]\n").is_none());
        assert!(parse_user_sprite(b"img=a\0b").is_none());
    }
}
