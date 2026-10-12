//! The `.milk2` double-preset container of MilkDrop 3 (MD3 3.37, `docs/md3.md` "Double presets").
//!
//! A `.milk2` is two complete `.milk` texts (`[PRESET1_BEGIN]` .. `[PRESET1_END]`, `[PRESET2_BEGIN]` ..
//! `[PRESET2_END]`) plus eight header keys that describe a frozen MilkDrop 2 soft cut between them:
//! `blending_pattern`, `blending_progress`, `blending_direction` and `random_1..5` (and the optional flag
//! `sprite=1`, after which a sprite block follows `[PRESET2_END]` and belongs to PRESET2).
//!
//! [`parse_milk2`] follows the MD3 loader exactly, quirks included:
//!
//! * the four markers are found by scanning the file in `fgets(buf, 255)` chunks, **counting chunks, not
//!   physical lines**; a line of 254 bytes or more (terminator included) counts twice and shifts every later
//!   marker, while the block extraction that follows counts real lines;
//! * the header keys are matched by substring on every `fgets(buf, 256)` chunk of the file, also inside the
//!   two blocks; the first key a chunk matches wins and later duplicates overwrite earlier ones;
//! * a header key that is missing keeps the value of the previous load. MD3 keeps those in process-global
//!   variables; here they are the explicit input [`Milk2Header`] (what the previous `.milk2` or ordinary soft
//!   cut left behind; [`Milk2Header::default`] is the process start);
//! * `sprite=1` extends the PRESET2 range to the last `[SPRITE1_END]` line;
//! * MD3 raises C++ exceptions for malformed numbers (`std::stof` / `std::stoi`); they are an
//!   [`Milk2Error`] here.
//!
//! Each half is plain `.milk` text for [`Preset::parse`](crate::Preset::parse). A file that has the `.milk2`
//! extension but fails the marker scan ([`Milk2Error::NotDouble`]) is loaded as an ordinary `.milk`.
//! [`write_milk2`] is the saver, byte for byte.

use std::fmt;

const P1_BEGIN: &[u8] = b"[PRESET1_BEGIN]";
const P1_END: &[u8] = b"[PRESET1_END]";
const P2_BEGIN: &[u8] = b"[PRESET2_BEGIN]";
const P2_END: &[u8] = b"[PRESET2_END]";
const SPRITE_END: &[u8] = b"[SPRITE1_END]";
const SPRITE_BEGIN: &str = "[SPRITE1_BEGIN]";

/// `fgets(buf, 255)`: the chunk size of the marker scan and the sprite-end scan.
const SCAN_CHUNK: usize = 255;
/// `fgets(buf, 256)`: the chunk size of the header key reader.
const HEADER_CHUNK: usize = 256;

/// The value of the eight header keys, in the form they are carried from one load to the next.
///
/// MD3 keeps these in globals: a `.milk2` that lacks a key keeps what the previous load left there, and every
/// ordinary soft cut overwrites all of them. [`Default`] is the state at process start.
#[derive(Clone, Debug, PartialEq)]
pub struct Milk2Header {
    /// `blending_pattern`: the pattern name (empty = none).
    pub pattern: String,
    /// `blending_progress`: where the frozen blend stands, used raw (not clamped).
    pub progress: f32,
    /// `blending_direction`: only `-1` is special.
    pub direction: i32,
    /// `random_1` .. `random_5`.
    pub random: [f32; 5],
}

impl Default for Milk2Header {
    fn default() -> Self {
        Self {
            pattern: String::new(),
            progress: 1.0,
            direction: 0,
            random: [0.0; 5],
        }
    }
}

/// 1-based line numbers of the four markers, as the marker scan counts them (see the module docs for the chunk
/// quirk).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MarkerLines {
    /// `[PRESET1_BEGIN]`.
    pub preset1_begin: u32,
    /// `[PRESET1_END]`.
    pub preset1_end: u32,
    /// `[PRESET2_BEGIN]`.
    pub preset2_begin: u32,
    /// `[PRESET2_END]` (the last `[SPRITE1_END]` line, or 0 if there is none, with `sprite=1`).
    pub preset2_end: u32,
}

/// A parsed `.milk2` file.
#[derive(Clone, Debug, PartialEq)]
pub struct Milk2 {
    /// `blending_pattern` (whitespace removed).
    pub pattern: String,
    /// `blending_progress`.
    pub progress: f32,
    /// `blending_direction`.
    pub direction: i32,
    /// `random_1` .. `random_5`.
    pub random: [f32; 5],
    /// The header has `sprite=1`: the text of PRESET2 includes the sprite block (see [`Milk2::preset2`]).
    pub has_sprite: bool,
    /// Name of PRESET1 (`NAME=` first line, a trailing `.milk` / `.milk2` removed); `None` = MD3 shows `Unknow`.
    pub name1: Option<String>,
    /// Name of PRESET2.
    pub name2: Option<String>,
    /// Text of PRESET1: the lines between its markers (its `NAME=` line included), each ended by `\n`.
    pub preset1: Vec<u8>,
    /// Text of PRESET2: the lines between its markers, each ended by `\n`. With `sprite=1` this runs up to (not
    /// including) the last `[SPRITE1_END]` line, so it also holds `[PRESET2_END]`, a blank line and the sprite
    /// block; MD3's preset importer ignores everything it does not know and the sprite parser reads the block.
    pub preset2: Vec<u8>,
    /// Where the marker scan found the markers.
    pub markers: MarkerLines,
    /// Non-fatal problems (MD3 logs "File does not have enough lines" and uses an empty text).
    pub warnings: Vec<String>,
}

impl Milk2 {
    /// The header as the next load's carry-over input.
    pub fn header(&self) -> Milk2Header {
        Milk2Header {
            pattern: self.pattern.clone(),
            progress: self.progress,
            direction: self.direction,
            random: self.random,
        }
    }

    /// Assemble a double preset from two `.milk` bodies (each without its `NAME=` line; the name goes in front).
    /// `sprite` is the sprite block's lines between `[SPRITE1_BEGIN]` and `[SPRITE1_END]` (LF-terminated), which
    /// makes `has_sprite` true. The result equals what [`parse_milk2`] returns for the written file.
    pub fn new(
        header: &Milk2Header,
        name1: &str,
        body1: &[u8],
        name2: &str,
        body2: &[u8],
        sprite: Option<&[u8]>,
    ) -> Self {
        let mut preset1 = Vec::new();
        push_block(&mut preset1, name1, body1);
        let mut preset2 = Vec::new();
        push_block(&mut preset2, name2, body2);
        if let Some(lines) = sprite {
            preset2.extend_from_slice(P2_END);
            preset2.extend_from_slice(b"\n\n");
            preset2.extend_from_slice(SPRITE_BEGIN.as_bytes());
            preset2.push(b'\n');
            preset2.extend_from_slice(lines);
            if !lines.is_empty() && !lines.ends_with(b"\n") {
                preset2.push(b'\n');
            }
        }
        let mut m = Milk2 {
            pattern: header.pattern.clone(),
            progress: header.progress,
            direction: header.direction,
            random: header.random,
            has_sprite: sprite.is_some(),
            name1: half_name(&preset1),
            name2: half_name(&preset2),
            preset1,
            preset2,
            markers: MarkerLines::default(),
            warnings: Vec::new(),
        };
        // the marker lines the saver's layout gives (the header is 3 + sprite + 1 + 1 + 1 + 5 + 1 lines)
        let head = 3 + u32::from(m.has_sprite) + 3 + 5 + 1;
        let l1 = count_lines(&m.preset1);
        let l2 = count_lines(&m.preset2);
        m.markers = MarkerLines {
            preset1_begin: head + 1,
            preset1_end: head + 2 + l1,
            preset2_begin: head + 4 + l1,
            preset2_end: head + 5 + l1 + l2,
        };
        m
    }
}

fn push_block(out: &mut Vec<u8>, name: &str, body: &[u8]) {
    out.extend_from_slice(b"NAME=");
    out.extend_from_slice(name.as_bytes());
    out.push(b'\n');
    out.extend_from_slice(body);
    if !body.is_empty() && !body.ends_with(b"\n") {
        out.push(b'\n');
    }
}

fn count_lines(text: &[u8]) -> u32 {
    text.iter().filter(|&&b| b == b'\n').count() as u32
}

/// Why [`parse_milk2`] failed.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Milk2Error {
    /// The marker scan did not find all four markers: not a double preset. A `.milk2` file is then loaded as an
    /// ordinary `.milk`.
    NotDouble,
    /// A header key's value has no number (`std::stof` / `std::stoi` throw `invalid_argument`).
    InvalidNumber {
        /// The key, e.g. `blending_progress`.
        key: &'static str,
    },
    /// A header key's number does not fit (`out_of_range`).
    NumberOutOfRange {
        /// The key.
        key: &'static str,
    },
}

impl fmt::Display for Milk2Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Milk2Error::NotDouble => f.write_str("not a double preset (a marker line is missing)"),
            Milk2Error::InvalidNumber { key } => write!(f, "{key}: not a number"),
            Milk2Error::NumberOutOfRange { key } => write!(f, "{key}: number out of range"),
        }
    }
}

impl std::error::Error for Milk2Error {}

/// `isMilk2(path)`: the last `.` of the path starts the suffix `.milk2`, case-sensitive.
pub fn is_milk2_path(path: &str) -> bool {
    path.rfind('.').is_some_and(|i| &path[i..] == ".milk2")
}

/// `fgets(buf, size)` chunks of `bytes`: at most `size - 1` bytes each, ending after a `\n`.
fn chunks(bytes: &[u8], size: usize) -> impl Iterator<Item = &[u8]> {
    let max = size - 1;
    let mut rest = bytes;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let upto = rest.len().min(max);
        let end = rest[..upto]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(upto, |i| i + 1);
        let (head, tail) = rest.split_at(end);
        rest = tail;
        Some(head)
    })
}

/// What `strstr` sees of a chunk: up to the first NUL.
fn c_str(chunk: &[u8]) -> &[u8] {
    let end = chunk.iter().position(|&b| b == 0).unwrap_or(chunk.len());
    &chunk[..end]
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// `FUN_14004b584`: the marker scan. `None` if EOF comes before all four are found.
fn scan_markers(bytes: &[u8]) -> Option<MarkerLines> {
    let mut m = [0u32; 4];
    for (n, chunk) in chunks(bytes, SCAN_CHUNK).enumerate() {
        let n = n as u32 + 1;
        let c = c_str(chunk);
        if contains(c, P1_BEGIN) {
            m[0] = n;
        } else if contains(c, P1_END) {
            m[1] = n;
        } else if contains(c, P2_BEGIN) {
            m[2] = n;
        } else if contains(c, P2_END) {
            m[3] = n;
        }
        if m.iter().all(|&v| v > 0) {
            return Some(MarkerLines {
                preset1_begin: m[0],
                preset1_end: m[1],
                preset2_begin: m[2],
                preset2_end: m[3],
            });
        }
    }
    None
}

/// Whether the marker scan finds all four markers: the file is a double preset.
pub fn is_double_preset(bytes: &[u8]) -> bool {
    scan_markers(bytes).is_some()
}

/// `FUN_14004b6b0`: the chunk number of the last `[SPRITE1_END]`, 0 if there is none.
fn scan_sprite_end(bytes: &[u8]) -> u32 {
    let mut last = 0;
    for (n, chunk) in chunks(bytes, SCAN_CHUNK).enumerate() {
        if contains(c_str(chunk), SPRITE_END) {
            last = n as u32 + 1;
        }
    }
    last
}

fn is_c_space(b: u8) -> bool {
    matches!(b, b' ' | 0x09..=0x0d)
}

/// `strtof` prefix (decimal, `inf`, `nan`) with `std::stof`'s failure modes.
fn stof(text: &[u8], key: &'static str) -> Result<f32, Milk2Error> {
    let s = &text[text
        .iter()
        .position(|&b| !is_c_space(b))
        .unwrap_or(text.len())..];
    let mut i = 0;
    if matches!(s.first(), Some(b'+' | b'-')) {
        i += 1;
    }
    let rest = &s[i..];
    let lower: Vec<u8> = rest.iter().take(8).map(u8::to_ascii_lowercase).collect();
    if lower.starts_with(b"infinity") || lower.starts_with(b"inf") || lower.starts_with(b"nan") {
        let end = i + if lower.starts_with(b"infinity") { 8 } else { 3 };
        return std::str::from_utf8(&s[..end])
            .ok()
            .and_then(|t| t.parse::<f32>().ok())
            .ok_or(Milk2Error::InvalidNumber { key });
    }
    let digits = |from: usize| s[from..].iter().take_while(|b| b.is_ascii_digit()).count();
    let int_len = digits(i);
    i += int_len;
    let mut frac_len = 0;
    if s.get(i) == Some(&b'.') {
        frac_len = digits(i + 1);
        if int_len + frac_len > 0 {
            i += 1 + frac_len;
        }
    }
    if int_len + frac_len == 0 {
        return Err(Milk2Error::InvalidNumber { key });
    }
    if matches!(s.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(s.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        let exp = digits(j);
        if exp > 0 {
            i = j + exp;
        }
    }
    let v: f32 = std::str::from_utf8(&s[..i])
        .ok()
        .and_then(|t| t.parse().ok())
        .ok_or(Milk2Error::InvalidNumber { key })?;
    if v.is_infinite() {
        return Err(Milk2Error::NumberOutOfRange { key });
    }
    Ok(v)
}

/// `std::stoi(text, nullptr, 10)`: leading blanks, a sign, the longest run of digits.
fn stoi(text: &[u8], key: &'static str) -> Result<i32, Milk2Error> {
    let s = &text[text
        .iter()
        .position(|&b| !is_c_space(b))
        .unwrap_or(text.len())..];
    let (neg, s) = match s.first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let n = s.iter().take_while(|b| b.is_ascii_digit()).count();
    if n == 0 {
        return Err(Milk2Error::InvalidNumber { key });
    }
    let mut v: i64 = 0;
    for &d in &s[..n] {
        v = v * 10 + i64::from(d - b'0');
        if v > i64::from(i32::MAX) + 1 {
            return Err(Milk2Error::NumberOutOfRange { key });
        }
    }
    let v = if neg { -v } else { v };
    i32::try_from(v).map_err(|_| Milk2Error::NumberOutOfRange { key })
}

struct Keys {
    sprite: bool,
    header: Milk2Header,
}

/// `FUN_14007ce6c`: the header key reader. Every `fgets(buf, 256)` chunk is tested in the order below, the first
/// key it contains is read from the text after the chunk's first `=`.
fn read_header(bytes: &[u8], previous: &Milk2Header) -> Result<Keys, Milk2Error> {
    const RANDOM: [&[u8]; 5] = [
        b"random_1=",
        b"random_2=",
        b"random_3=",
        b"random_4=",
        b"random_5=",
    ];
    const RANDOM_KEY: [&str; 5] = ["random_1", "random_2", "random_3", "random_4", "random_5"];
    let mut k = Keys {
        sprite: false,
        header: previous.clone(),
    };
    for chunk in chunks(bytes, HEADER_CHUNK) {
        let c = c_str(chunk);
        // MD3 takes the value after the first `=` of the chunk, whichever key matched
        let value = || {
            let at = c.iter().position(|&b| b == b'=').map_or(c.len(), |i| i + 1);
            &c[at..]
        };
        if contains(c, b"sprite=1") {
            k.sprite = true;
        } else if contains(c, b"blending_pattern=") {
            let text: Vec<u8> = value()
                .iter()
                .copied()
                .filter(|&b| !is_c_space(b))
                .collect();
            k.header.pattern = String::from_utf8_lossy(&text).into_owned();
        } else if contains(c, b"blending_progress=") {
            k.header.progress = stof(value(), "blending_progress")?;
        } else if contains(c, b"blending_direction=") {
            k.header.direction = stoi(value(), "blending_direction")?;
        } else if let Some(i) = RANDOM.iter().position(|key| contains(c, key)) {
            k.header.random[i] = stof(value(), RANDOM_KEY[i])?;
        }
    }
    Ok(k)
}

/// The physical lines of the file as the text-mode `getline` loop sees them: split at `\n`, one `\r` before it
/// dropped; a final line without terminator counts.
fn physical_lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        match rest.iter().position(|&b| b == b'\n') {
            Some(i) => {
                let l = &rest[..i];
                out.push(l.strip_suffix(b"\r").unwrap_or(l));
                rest = &rest[i + 1..];
            }
            None => {
                out.push(rest);
                break;
            }
        }
    }
    out
}

/// `FUN_140049c14`: lines `first..=last` (1-based, inclusive), each ended by `\n`. Fewer lines than `last` is a
/// warning and an empty text.
fn read_lines(
    lines: &[&[u8]],
    first: i64,
    last: i64,
    what: &str,
    warnings: &mut Vec<String>,
) -> Vec<u8> {
    if (lines.len() as i64) < last {
        warnings.push(format!("{what}: file does not have enough lines"));
        return Vec::new();
    }
    let mut out = Vec::new();
    for n in first.max(1)..=last {
        out.extend_from_slice(lines[(n - 1) as usize]);
        out.push(b'\n');
    }
    out
}

/// `NAME=` first line of a half: the name without a trailing `.milk` / `.milk2`.
fn half_name(text: &[u8]) -> Option<String> {
    let first = text.split(|&b| b == b'\n').next().unwrap_or(&[]);
    let rest = first.strip_prefix(b"NAME=")?;
    let s = String::from_utf8_lossy(rest);
    let s = s
        .strip_suffix(".milk2")
        .or_else(|| s.strip_suffix(".milk"))
        .unwrap_or(&s);
    Some(s.to_owned())
}

/// Parse a `.milk2` file like MD3's loader. `previous` supplies the header keys the file does not have.
///
/// Returns [`Milk2Error::NotDouble`] when a marker is missing (load the file as a plain `.milk` then) and the
/// other errors for a malformed number in a header key.
pub fn parse_milk2(bytes: &[u8], previous: &Milk2Header) -> Result<Milk2, Milk2Error> {
    let mut markers = scan_markers(bytes).ok_or(Milk2Error::NotDouble)?;
    let keys = read_header(bytes, previous)?;
    if keys.sprite {
        markers.preset2_end = scan_sprite_end(bytes);
    }
    let lines = physical_lines(bytes);
    let mut warnings = Vec::new();
    let r = |b: u32, e: u32, what: &str, w: &mut Vec<String>| {
        read_lines(&lines, i64::from(b) + 1, i64::from(e) - 1, what, w)
    };
    let preset1 = r(
        markers.preset1_begin,
        markers.preset1_end,
        "PRESET1",
        &mut warnings,
    );
    let preset2 = r(
        markers.preset2_begin,
        markers.preset2_end,
        "PRESET2",
        &mut warnings,
    );
    Ok(Milk2 {
        name1: half_name(&preset1),
        name2: half_name(&preset2),
        pattern: keys.header.pattern,
        progress: keys.header.progress,
        direction: keys.header.direction,
        random: keys.header.random,
        has_sprite: keys.sprite,
        preset1,
        preset2,
        markers,
        warnings,
    })
}

/// Line terminator of [`write_milk2`]. MD3 writes through a text-mode stream, so its files are CRLF on Windows;
/// the loader reads either.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnding {
    /// `\n`.
    Lf,
    /// `\r\n`.
    Crlf,
}

/// The `.milk2` saver. `created` is the timestamp of the first line (`dd/mm/YYYY at HH:MM:SS`, local time);
/// everything else is determined by `m`. [`parse_milk2`] reads the result back into an equal [`Milk2`].
pub fn write_milk2(m: &Milk2, created: &str, eol: LineEnding) -> Vec<u8> {
    let mut lf = Vec::with_capacity(m.preset1.len() + m.preset2.len() + 512);
    let mut line = |s: &str| {
        lf.extend_from_slice(s.as_bytes());
        lf.push(b'\n');
    };
    line(&format!("Created with MILKDROP 3.37 on {created}"));
    line("github.com/M0Rf30/lactea");
    line("");
    if m.has_sprite {
        line("sprite=1");
    }
    line(&format!("blending_pattern={}", m.pattern));
    line(&format!("blending_progress={:.2}", f64::from(m.progress)));
    line(&format!("blending_direction={}", m.direction));
    for (i, r) in m.random.iter().enumerate() {
        line(&format!("random_{}={:.6}", i + 1, f64::from(*r)));
    }
    line("");
    line("[PRESET1_BEGIN]");
    lf.extend_from_slice(&m.preset1);
    let mut line = |s: &str| {
        lf.extend_from_slice(s.as_bytes());
        lf.push(b'\n');
    };
    line("[PRESET1_END]");
    line("");
    line("[PRESET2_BEGIN]");
    lf.extend_from_slice(&m.preset2);
    let tail = if m.has_sprite {
        "[SPRITE1_END]"
    } else {
        "[PRESET2_END]"
    };
    lf.extend_from_slice(tail.as_bytes());
    lf.push(b'\n');
    match eol {
        LineEnding::Lf => lf,
        LineEnding::Crlf => {
            let mut out = Vec::with_capacity(lf.len() + lf.len() / 20);
            for &b in &lf {
                if b == b'\n' {
                    out.push(b'\r');
                }
                out.push(b);
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block of `n` lines: `NAME=`, version, section, then filler code lines.
    fn block(name: &str, n: usize) -> String {
        let mut s = format!("NAME={name}\nMILKDROP_PRESET_VERSION=201\n[preset00]\n");
        let mut i = 0;
        while s.matches('\n').count() < n {
            s.push_str(&format!("per_frame_{i}=q1 = {i};\n"));
            i += 1;
        }
        s
    }

    fn crlf(s: &str) -> Vec<u8> {
        s.replace('\n', "\r\n").into_bytes()
    }

    /// The structure of the pack's `000.milk2`: 2 comment lines, header, markers at 13 / 414 / 416 / 1335, blocks of
    /// 400 and 918 lines, CRLF, no trailing blank line.
    fn file_000() -> Vec<u8> {
        let mut s = String::new();
        s.push_str("Created with MILKDROP 3.2 on 12/07/2023 at 23:12:27\nhttps://twitter.com/MilkDrop2077\n\n");
        s.push_str("blending_pattern=cercle\nblending_progress=0.45\nblending_direction=-1\n");
        s.push_str("random_1=0.579675\nrandom_2=0.335230\nrandom_3=0.944580\nrandom_4=0.292683\nrandom_5=0.528049\n\n");
        s.push_str("[PRESET1_BEGIN]\n");
        s.push_str(&block("NeonAngel vs Flexi - CyberPunk Rain Move.milk", 400));
        s.push_str("[PRESET1_END]\n\n[PRESET2_BEGIN]\n");
        s.push_str(&block("MilkDrop2077.026 My Little Forest 003 RedMove", 918));
        s.push_str("[PRESET2_END]\n");
        crlf(&s)
    }

    #[test]
    fn parses_the_structure_of_a_real_file() {
        let bytes = file_000();
        let m = parse_milk2(&bytes, &Milk2Header::default()).unwrap();
        assert_eq!(m.pattern, "cercle");
        assert_eq!(m.progress, 0.45f32);
        assert_eq!(m.direction, -1);
        assert_eq!(m.random, [0.579675, 0.335230, 0.944580, 0.292683, 0.528049]);
        assert!(!m.has_sprite);
        assert_eq!(
            m.markers,
            MarkerLines {
                preset1_begin: 13,
                preset1_end: 414,
                preset2_begin: 416,
                preset2_end: 1335
            }
        );
        assert_eq!(count_lines(&m.preset1), 400);
        assert_eq!(count_lines(&m.preset2), 918);
        assert!(
            m.preset1
                .starts_with(b"NAME=NeonAngel vs Flexi - CyberPunk Rain Move.milk\n")
        );
        assert!(
            m.preset2
                .starts_with(b"NAME=MilkDrop2077.026 My Little Forest 003 RedMove\n")
        );
        assert!(!m.preset1.contains(&b'\r'), "texts are LF");
        assert_eq!(
            m.name1.as_deref(),
            Some("NeonAngel vs Flexi - CyberPunk Rain Move")
        );
        assert_eq!(
            m.name2.as_deref(),
            Some("MilkDrop2077.026 My Little Forest 003 RedMove")
        );
        assert!(m.warnings.is_empty());
        // both halves are plain `.milk` for the ordinary importer
        let p1 = crate::Preset::parse(&m.preset1).unwrap();
        assert_eq!(p1.milkdrop_preset_version, 201);
    }

    #[test]
    fn tolerant_header_and_markers() {
        // spec A TV2: key lines anywhere, spaces inside the pattern name, a missing PRESET2 NAME
        let f = "hello\nblending_pattern= cer cle  \nblending_direction=-1\nrandom_2=0.25\n[PRESET1_BEGIN]\nNAME=a.milk\nMILKDROP_PRESET_VERSION=201\n[preset00]\nfDecay=0.9\n[PRESET1_END]\n[PRESET2_BEGIN]\nMILKDROP_PRESET_VERSION=201\n[preset00]\nfDecay=0.5\n[PRESET2_END]\n";
        let prev = Milk2Header {
            pattern: "side".into(),
            progress: 0.7,
            direction: 1,
            random: [0.1, 0.2, 0.3, 0.4, 0.5],
        };
        let m = parse_milk2(f.as_bytes(), &prev).unwrap();
        assert_eq!(m.pattern, "cercle");
        assert_eq!(m.direction, -1);
        assert_eq!(
            m.random,
            [0.1, 0.25, 0.3, 0.4, 0.5],
            "missing keys keep the previous values"
        );
        assert_eq!(m.progress, 0.7);
        assert_eq!(
            m.markers,
            MarkerLines {
                preset1_begin: 5,
                preset1_end: 10,
                preset2_begin: 11,
                preset2_end: 15
            }
        );
        assert_eq!(
            m.preset1,
            b"NAME=a.milk\nMILKDROP_PRESET_VERSION=201\n[preset00]\nfDecay=0.9\n"
        );
        assert_eq!(
            m.preset2,
            b"MILKDROP_PRESET_VERSION=201\n[preset00]\nfDecay=0.5\n"
        );
        assert_eq!(m.name1.as_deref(), Some("a"));
        assert_eq!(m.name2, None);
        // the process-start defaults when nothing was loaded before
        let m = parse_milk2(f.as_bytes(), &Milk2Header::default()).unwrap();
        assert_eq!((m.progress, m.random[0], m.random[2]), (1.0, 0.0, 0.0));
    }

    #[test]
    fn missing_marker_is_not_double() {
        let f =
            "blending_pattern=zoom\n[PRESET1_BEGIN]\nx=1\n[PRESET1_END]\n[PRESET2_BEGIN]\nx=2\n";
        assert_eq!(
            parse_milk2(f.as_bytes(), &Milk2Header::default()),
            Err(Milk2Error::NotDouble)
        );
        assert!(!is_double_preset(f.as_bytes()));
        assert!(is_double_preset(&file_000()));
        // markers match by substring and are tested in order within a chunk (BEGIN wins over END)
        let f = "[PRESET1_BEGIN] [PRESET1_END]\n[PRESET1_END]\n[PRESET2_BEGIN]\n[PRESET2_END]\n";
        let m = parse_milk2(f.as_bytes(), &Milk2Header::default()).unwrap();
        assert_eq!((m.markers.preset1_begin, m.markers.preset1_end), (1, 2));
    }

    #[test]
    fn marker_order_is_not_checked() {
        let f = "[PRESET2_END]\n[PRESET2_BEGIN]\n[PRESET1_END]\n[PRESET1_BEGIN]\nx\n";
        let m = parse_milk2(f.as_bytes(), &Milk2Header::default()).unwrap();
        assert_eq!(
            (m.markers.preset1_begin, m.markers.preset2_end),
            (4, 1),
            "END before BEGIN is accepted"
        );
        assert!(m.preset1.is_empty() && m.preset2.is_empty());
    }

    #[test]
    fn a_long_line_counts_twice_in_the_marker_scan() {
        let long = "x".repeat(300);
        let plain = "[PRESET1_BEGIN]\na\n[PRESET1_END]\n[PRESET2_BEGIN]\nb\n[PRESET2_END]\n";
        let f = format!("{long}\n{plain}");
        let m = parse_milk2(f.as_bytes(), &Milk2Header::default()).unwrap();
        // 301 bytes with the terminator: two `fgets(255)` chunks; the real file has the markers at 2 / 4 / 5 / 7
        assert_eq!(
            m.markers,
            MarkerLines {
                preset1_begin: 3,
                preset1_end: 5,
                preset2_begin: 6,
                preset2_end: 8
            }
        );
        // the extraction counts real lines (7, not 8): every range is shifted by one line
        assert_eq!(m.preset1, b"[PRESET1_END]\n");
        assert_eq!(m.preset2, b"[PRESET2_END]\n");
        assert!(m.warnings.is_empty());
        // 253 bytes + "\r\n" = 255 bytes: also split; 252 + CRLF = 254: one chunk
        let l253 = "y".repeat(253);
        let m = parse_milk2(
            crlf(&format!("{l253}\n{plain}")).as_slice(),
            &Milk2Header::default(),
        )
        .unwrap();
        assert_eq!(m.markers.preset1_begin, 3);
        let l252 = "y".repeat(252);
        let m = parse_milk2(
            crlf(&format!("{l252}\n{plain}")).as_slice(),
            &Milk2Header::default(),
        )
        .unwrap();
        assert_eq!(m.markers.preset1_begin, 2);
    }

    #[test]
    fn sprite_flag_extends_preset2_to_the_last_sprite_end() {
        let f = "sprite=1\nblending_pattern=side\n[PRESET1_BEGIN]\na\n[PRESET1_END]\n\n[PRESET2_BEGIN]\nNAME=p2\nb\n[PRESET2_END]\n\n[SPRITE1_BEGIN]\nSpriteName=x.png\ncode_1=a=1;\n[SPRITE1_END]\n";
        let m = parse_milk2(f.as_bytes(), &Milk2Header::default()).unwrap();
        assert!(m.has_sprite);
        assert_eq!(m.markers.preset2_end, 15);
        assert_eq!(
            m.preset2,
            b"NAME=p2\nb\n[PRESET2_END]\n\n[SPRITE1_BEGIN]\nSpriteName=x.png\ncode_1=a=1;\n"
        );
        // without the flag the sprite block is ignored
        let g = f.replace("sprite=1\n", "");
        let m = parse_milk2(g.as_bytes(), &Milk2Header::default()).unwrap();
        assert!(!m.has_sprite);
        assert_eq!(m.preset2, b"NAME=p2\nb\n");
        // flag but no sprite end: PRESET2 is empty
        let g = f.replace("[SPRITE1_END]\n", "");
        let m = parse_milk2(g.as_bytes(), &Milk2Header::default()).unwrap();
        assert_eq!(m.markers.preset2_end, 0);
        assert!(m.preset2.is_empty());
    }

    #[test]
    fn header_keys_match_anywhere_and_the_last_wins() {
        let f = "blending_progress=0.2\n[PRESET1_BEGIN]\nx\nblending_progress= 0.8abc\nblending_direction=1.00\nfoo=1 random_3=0.5\n[PRESET1_END]\n[PRESET2_BEGIN]\ny\n[PRESET2_END]\n";
        let m = parse_milk2(f.as_bytes(), &Milk2Header::default()).unwrap();
        assert_eq!(
            m.progress, 0.8,
            "duplicates: last wins, trailing junk ignored"
        );
        assert_eq!(m.direction, 1, "stoi stops at the dot");
        // the value is read after the chunk's first `=`, whichever key matched
        assert_eq!(
            m.random[2], 1.0,
            "'foo=1 random_3=0.5' reads the text after the first '=' ('1 random_3=0.5')"
        );
        let g = f.replace("foo=1 random_3=0.5", "random_3=0.5");
        assert_eq!(
            parse_milk2(g.as_bytes(), &Milk2Header::default())
                .unwrap()
                .random[2],
            0.5
        );
    }

    #[test]
    fn malformed_numbers_are_errors() {
        let mk = |k: &str, v: &str| {
            format!("{k}={v}\n[PRESET1_BEGIN]\n[PRESET1_END]\n[PRESET2_BEGIN]\n[PRESET2_END]\n")
        };
        let d = Milk2Header::default();
        assert_eq!(
            parse_milk2(mk("blending_progress", "abc").as_bytes(), &d),
            Err(Milk2Error::InvalidNumber {
                key: "blending_progress"
            })
        );
        assert_eq!(
            parse_milk2(mk("random_4", "").as_bytes(), &d),
            Err(Milk2Error::InvalidNumber { key: "random_4" })
        );
        assert_eq!(
            parse_milk2(mk("blending_direction", "x").as_bytes(), &d),
            Err(Milk2Error::InvalidNumber {
                key: "blending_direction"
            })
        );
        assert_eq!(
            parse_milk2(mk("blending_direction", "99999999999").as_bytes(), &d),
            Err(Milk2Error::NumberOutOfRange {
                key: "blending_direction"
            })
        );
        assert_eq!(
            parse_milk2(mk("random_1", "1e99").as_bytes(), &d),
            Err(Milk2Error::NumberOutOfRange { key: "random_1" })
        );
        // forms strtof / strtol accept
        let m = parse_milk2(mk("random_5", "  -.5e1xyz").as_bytes(), &d).unwrap();
        assert_eq!(m.random[4], -5.0);
        let m = parse_milk2(mk("blending_direction", " +7").as_bytes(), &d).unwrap();
        assert_eq!(m.direction, 7);
    }

    #[test]
    fn extension_check_is_the_last_suffix() {
        assert!(is_milk2_path("a/b/x.milk2"));
        assert!(is_milk2_path("x.y.milk2"));
        assert!(!is_milk2_path("x.milk"));
        assert!(!is_milk2_path("x.MILK2"));
        assert!(!is_milk2_path("x.milk2.bak"));
        assert!(!is_milk2_path("milk2"));
    }

    #[test]
    fn lf_and_crlf_read_the_same() {
        let crlf_bytes = file_000();
        let lf: Vec<u8> = String::from_utf8(crlf_bytes.clone())
            .unwrap()
            .replace("\r\n", "\n")
            .into_bytes();
        let a = parse_milk2(&crlf_bytes, &Milk2Header::default()).unwrap();
        let b = parse_milk2(&lf, &Milk2Header::default()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn writer_reproduces_the_saver_layout() {
        let h = Milk2Header {
            pattern: "cercle".into(),
            progress: 0.45,
            direction: -1,
            random: [0.579675, 0.335230, 0.944580, 0.292683, 0.528049],
        };
        let m = Milk2::new(
            &h,
            "name1",
            b"MILKDROP_PRESET_VERSION=201\n",
            "name2",
            b"fDecay=0.5\n",
            None,
        );
        let out = write_milk2(&m, "05/01/2026 at 12:34:56", LineEnding::Lf);
        let want = "Created with MILKDROP 3.37 on 05/01/2026 at 12:34:56\ngithub.com/M0Rf30/lactea\n\nblending_pattern=cercle\nblending_progress=0.45\nblending_direction=-1\nrandom_1=0.579675\nrandom_2=0.335230\nrandom_3=0.944580\nrandom_4=0.292683\nrandom_5=0.528049\n\n[PRESET1_BEGIN]\nNAME=name1\nMILKDROP_PRESET_VERSION=201\n[PRESET1_END]\n\n[PRESET2_BEGIN]\nNAME=name2\nfDecay=0.5\n[PRESET2_END]\n";
        assert_eq!(String::from_utf8(out.clone()).unwrap(), want);
        let crlf_out = write_milk2(&m, "05/01/2026 at 12:34:56", LineEnding::Crlf);
        assert_eq!(crlf_out, crlf(want));
        // reading the file back gives the same model, including the marker lines `new` predicted
        assert_eq!(parse_milk2(&out, &Milk2Header::default()).unwrap(), m);
        assert_eq!(parse_milk2(&crlf_out, &Milk2Header::default()).unwrap(), m);
    }

    #[test]
    fn writer_round_trips_files_byte_for_byte() {
        // a file the saver would have written (CRLF), 3.37 layout, with and without a sprite
        for sprite in [
            None,
            Some(&b"SpriteName=a.png\nSpriteLayer=2\ncode_1=x=1;\n"[..]),
        ] {
            let h = Milk2Header {
                pattern: "plasma2".into(),
                progress: 0.35,
                direction: 1,
                random: [0.25, 0.5, 0.75, 0.125, 0.875],
            };
            let body1 = block("ignored", 30).split_once('\n').unwrap().1.to_owned();
            let body2 = block("ignored", 50).split_once('\n').unwrap().1.to_owned();
            let m = Milk2::new(&h, "one", body1.as_bytes(), "two", body2.as_bytes(), sprite);
            let file = write_milk2(&m, "12/07/2023 at 23:12:27", LineEnding::Crlf);
            let back = parse_milk2(&file, &Milk2Header::default()).unwrap();
            assert_eq!(back, m, "sprite: {}", sprite.is_some());
            assert_eq!(back.has_sprite, sprite.is_some());
            assert_eq!(
                write_milk2(&back, "12/07/2023 at 23:12:27", LineEnding::Crlf),
                file
            );
        }
        // and the 000.milk2-shaped file (it has the header's two comment lines, which the saver rewrites)
        let m = parse_milk2(&file_000(), &Milk2Header::default()).unwrap();
        let out = write_milk2(&m, "12/07/2023 at 23:12:27", LineEnding::Crlf);
        let again = parse_milk2(&out, &Milk2Header::default()).unwrap();
        assert_eq!(again.preset1, m.preset1);
        assert_eq!(again.preset2, m.preset2);
        assert_eq!(again.header(), m.header());
    }
}
