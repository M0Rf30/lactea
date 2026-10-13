//! Byte-exact port of MD2's preset line reader (`state.cpp`: `_GetLineByName`,
//! `GetFastInt/Float/String`, `ReadCode`).
//!
//! MD2 does not parse `.milk` as INI.  On the first lookup it scans the whole file once,
//! recording for every line `name<sep>rest` (sep = first `=` or space) the byte offset of
//! `rest`.  Later lookups find a name through a *cursor* (the line after the previous hit)
//! and otherwise by linear search from the top.  Every quirk of that scheme is reproduced
//! here.

use crate::cnum;
use std::collections::HashMap;

/// `MAX_BIGSTRING_LEN`
const MAX_BIGSTRING_LEN: usize = 32768;
/// `LINEFEED_CONTROL_CHAR`: terminates every line of a joined code block.
pub const LINEFEED_CONTROL_CHAR: char = '\u{1}';
const LF: u8 = 1;
/// `PresetFileParser::maxFileSize`: projectM refuses larger presets.
pub const PROJECTM_MAX_FILE_SIZE: usize = 0x100000;
/// Highest `prefixN` index `PresetFileParser::GetCode` reads (`index <= 99999`).
const PROJECTM_MAX_CODE_LINE: i32 = 99999;

/// `MAX_VARNAME_LEN-3`: name characters kept by the scan.
const NAME_CAP: usize = 125;
/// The first lookup MD2 ever makes (`GetFastInt` with `char buf[256]`) fixes the scan's
/// `fgets(szRet, nMaxRetChars-3)` limit to 252 -> at most 251 characters per scanned line.
const SCAN_FGETS_CHARS: usize = 251;

struct Entry {
    name: Vec<u8>,
    bytepos: usize,
}

/// Which importer's line semantics a [`Lookup`] follows.
enum Backend {
    /// MD2's `_GetLineByName` scan with its cursor.
    Md2 { entries: Vec<Entry>, cursor: usize },
    /// projectM's `PresetFileParser`: one `std::map` of lower-cased keys, first occurrence wins.
    ProjectM {
        map: HashMap<Vec<u8>, (usize, usize)>,
    },
}

pub struct Lookup<'a> {
    data: &'a [u8],
    backend: Backend,
}

/// `char ch = fgetc(f)` compared against `EOF` on a signed-char platform: a real EOF and a
/// 0xFF byte are indistinguishable.
#[inline]
fn is_eof(c: Option<u8>) -> bool {
    matches!(c, None | Some(0xFF))
}

impl<'a> Lookup<'a> {
    /// First-call scan of `_GetLineByName`.
    pub fn new(data: &'a [u8]) -> Self {
        let mut entries = Vec::new();
        let mut pos = 0usize;
        let getc = |pos: &mut usize| -> Option<u8> {
            let c = data.get(*pos).copied();
            if c.is_some() {
                *pos += 1;
            }
            c
        };
        loop {
            let mut name: Vec<u8> = Vec::new();
            let mut ch;
            loop {
                ch = getc(&mut pos);
                if name.len() < NAME_CAP {
                    name.push(ch.unwrap_or(0xFF));
                }
                if matches!(ch, Some(b'\r') | Some(b'\n') | Some(b' ') | Some(b'=')) || is_eof(ch) {
                    break;
                }
            }
            if is_eof(ch) {
                break;
            }
            if ch == Some(b'=') || ch == Some(b' ') {
                // szThisLineVarName[pos-1] = 0
                name.pop();
                if let Some(z) = name.iter().position(|&b| b == 0) {
                    name.truncate(z);
                }
                entries.push(Entry { name, bytepos: pos });
                // fgets(szRet, 252): up to 251 chars, stops after '\n'
                let mut n = 0;
                while n < SCAN_FGETS_CHARS {
                    match getc(&mut pos) {
                        None => break,
                        Some(b'\n') => break,
                        Some(_) => n += 1,
                    }
                }
            }
            // eat extra linefeed chars
            let c = loop {
                let c = getc(&mut pos);
                if !matches!(c, Some(b'\r') | Some(b'\n')) {
                    break c;
                }
            };
            if is_eof(c) {
                break;
            }
            pos -= 1; // back up one char
        }
        Lookup {
            data,
            backend: Backend::Md2 { entries, cursor: 0 },
        }
    }

    /// Follow projectM's `PresetFileParser` instead of MD2's scan (see [`Backend`]).
    pub fn is_projectm(&self) -> bool {
        matches!(self.backend, Backend::ProjectM { .. })
    }

    /// projectM backend with no key read (always false for the MD2 scan).
    pub fn is_empty(&self) -> bool {
        matches!(&self.backend, Backend::ProjectM { map } if map.is_empty())
    }

    /// `PresetFileParser::Read`: every non-empty line (split at `\r`/`\n`) is `key<sep>value`
    /// with `sep` = first space or `=` (a line without one, or starting with one, is skipped);
    /// the key is ASCII-lower-cased and only the first occurrence of a key is kept.
    pub fn projectm(data: &'a [u8]) -> Self {
        let mut map: HashMap<Vec<u8>, (usize, usize)> = HashMap::new();
        let mut start = 0usize;
        for end in 0..=data.len() {
            if end < data.len() && !matches!(data[end], b'\r' | b'\n') {
                continue;
            }
            let line = &data[start..end];
            if let Some(d) = line.iter().position(|&b| b == b' ' || b == b'=')
                && d > 0
            {
                let key = line[..d].to_ascii_lowercase();
                map.entry(key).or_insert((start + d + 1, end));
            }
            start = end + 1;
        }
        Lookup {
            data,
            backend: Backend::ProjectM { map },
        }
    }

    /// Tail of `_GetLineByName`: cursor / linear search, then read the value.
    /// `max_chars` is `nMaxRetChars - 3`.
    fn get_line(&mut self, name: &str, max_chars: usize) -> Option<Vec<u8>> {
        let nb = name.as_bytes();
        let (entries, cursor) = match &mut self.backend {
            Backend::ProjectM { map } => {
                let (s, e) = *map.get(&nb.to_ascii_lowercase())?;
                return Some(self.data[s..e].to_vec());
            }
            Backend::Md2 { entries, cursor } => (entries, cursor),
        };
        let cur_ok = *cursor < entries.len() && entries[*cursor].name == nb;
        if !cur_ok {
            let i = entries.iter().position(|e| e.name == nb)?;
            *cursor = i;
        }
        let mut p = entries[*cursor].bytepos;
        let mut out = Vec::new();
        while out.len() < max_chars {
            let c = self.data.get(p).copied();
            p += 1;
            if matches!(c, Some(b'\r') | Some(b'\n')) || is_eof(c) {
                break;
            }
            let Some(c) = c else { break };
            out.push(c);
        }
        // used as a C string afterwards
        if let Some(z) = out.iter().position(|&b| b == 0) {
            out.truncate(z);
        }
        *cursor += 1;
        Some(out)
    }

    /// `GetFastInt` (MD2: `sscanf %d`) / `GetInt` (projectM: `std::stoi`)
    pub fn int(&mut self, name: &str, def: i32) -> i32 {
        let pm = self.is_projectm();
        match self.get_line(name, 255 - 3) {
            Some(b) if pm => cnum::stoi(&b).unwrap_or(def),
            Some(b) => cnum::scan_int(&b).unwrap_or(def),
            None => def,
        }
    }

    /// `GetFastFloat` (MD2: `sscanf %f`) / `GetFloat` (projectM: `std::stof`)
    pub fn float(&mut self, name: &str, def: f32) -> f32 {
        let pm = self.is_projectm();
        match self.get_line(name, 255 - 3) {
            Some(b) if pm => cnum::stof(&b).unwrap_or(def),
            Some(b) => cnum::scan_float(&b).unwrap_or(def),
            None => def,
        }
    }

    /// MD2 `GetFastInt(...) != 0`; projectM `GetBool` is `GetInt(...) > 0`.
    pub fn flag(&mut self, name: &str, def: bool) -> bool {
        let v = self.int(name, def as i32);
        if self.is_projectm() { v > 0 } else { v != 0 }
    }

    /// An `int` member that projectM stores as `bool` (`GetBool`: value `> 0`) and MD2 keeps
    /// as `int`.
    pub fn flag_int(&mut self, name: &str, def: i32) -> i32 {
        let v = self.int(name, def);
        if self.is_projectm() {
            (v > 0) as i32
        } else {
            v
        }
    }

    /// `GetFastString` with `nMaxChars = MAX_BIGSTRING_LEN` (value limit 32764 chars);
    /// projectM: the whole value.
    fn string(&mut self, name: &str) -> Option<Vec<u8>> {
        self.get_line(name, MAX_BIGSTRING_LEN - 1 - 3)
    }

    /// A plain string value (projectM `GetString`): the whole value after the separator, empty when
    /// the key is missing.
    pub fn text(&mut self, name: &str) -> Vec<u8> {
        self.string(name).unwrap_or_default()
    }

    /// `ReadCode`: join `prefix1`, `prefix2`, ... (stop at the first missing key) with
    /// `\x01` terminators; a leading backtick on a line is stripped.
    ///
    /// projectM (`GetCode`): same join, but a literal `~!@#$` value is an ordinary line, there
    /// is no length cap, and the *first* copy of a duplicated key is the one read (so a stray
    /// `per_frame_8 ` line blanks the real `per_frame_8=...` further down).
    pub fn read_code(&mut self, prefix: &str) -> Vec<u8> {
        let pm = self.is_projectm();
        let mut out: Vec<u8> = Vec::new();
        let mut char_pos: i64 = 0;
        let mut line = 1;
        loop {
            let key = format!("{prefix}{line}");
            let val = self.string(&key);
            let (done, len) = match &val {
                None => (true, 0usize),
                Some(v) => {
                    let len = v.len();
                    // missing key -> "~!@#$"; a literal value "~!@#$" also ends the block
                    let end = !pm
                        && (v == b"~!@#$"
                            || (len as i64) >= (MAX_BIGSTRING_LEN as i64) - 1 - char_pos - 1);
                    (end, len)
                }
            };
            if done {
                break;
            }
            let Some(v) = val else { break };
            let mut l = len as i64;
            if v.first() == Some(&b'`') {
                out.extend_from_slice(&v[1..]);
                l -= 1;
            } else {
                out.extend_from_slice(&v);
            }
            out.push(LF);
            char_pos += l + 1;
            line += 1;
            if pm && line > PROJECTM_MAX_CODE_LINE {
                break;
            }
        }
        out
    }
}

/// Decode bytes from a preset: valid UTF-8 as is, anything else as Latin-1 (ANSI code page
/// stand-in).
pub fn decode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

/// Sane line scanner used only for the raw unknown-key map: `key<sep>value` per line,
/// `[section]` headers and separator-less lines skipped.
pub fn sane_entries(data: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    for raw in data.split(|&b| b == b'\n') {
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        let Some(sep) = line.iter().position(|&b| b == b'=' || b == b' ') else {
            continue;
        };
        let key = &line[..sep];
        let mut val = &line[sep + 1..];
        if let Some(r) = val.iter().position(|&b| b == b'\r') {
            val = &val[..r];
        }
        let nul = |s: &[u8]| s.iter().position(|&b| b == 0).unwrap_or(s.len());
        let (key, val) = (&key[..nul(key)], &val[..nul(val)]);
        if key.is_empty() {
            continue;
        }
        out.push((key.to_vec(), val.to_vec()));
    }
    out
}
