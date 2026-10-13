//! Scanner.l re-implementation (flex longest-match / first-rule semantics, 8-bit clean).

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Tok {
    Num(f64),
    Var(String),
    Func(String),
    Gmem,
    AddOp,
    SubOp,
    ModOp,
    OrOp,
    AndOp,
    DivOp,
    MulOp,
    PowOp,
    Equal,
    BelEq,
    AboEq,
    NotEq,
    BoolOr,
    BoolAnd,
    /// Single character token: `< > + - * / % ^ & | ! = ( ) [ ] ? : , ;`
    Ch(u8),
    /// Anything the scanner rejects (`PRJM_EVAL_UNDEF`).
    Undef(u8),
    Eof,
}

#[derive(Clone, Debug)]
pub(crate) struct Token {
    pub tok: Tok,
    pub line: u32,
    /// 0-based byte column within the line.
    pub col: u32,
    pub len: u32,
}

fn is_digit(c: u8) -> bool {
    c.is_ascii_digit()
}

fn is_name_start(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphabetic()
}

fn is_name_char(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphanumeric()
}

/// Length of `[eE][+-]?DIGIT+` at `i`, or 0.
fn expo_len(b: &[u8], i: usize) -> usize {
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        let ds = j;
        while j < b.len() && is_digit(b[j]) {
            j += 1;
        }
        if j > ds {
            return j - i;
        }
    }
    0
}

/// `isspace` in the C locale.
pub(crate) fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// Leading hex digits of `s` as `strtoul(.., 16)` reads them: the value saturated at `max`, whether
/// it saturated, and the number of digits consumed.
pub(crate) fn hex_prefix(s: &[u8], max: u64) -> (u64, bool, usize) {
    let mut v: u64 = 0;
    let mut over = false;
    let mut used = 0;
    for &d in s {
        let Some(n) = (d as char).to_digit(16) else {
            break;
        };
        used += 1;
        if !over {
            match v
                .checked_mul(16)
                .and_then(|x| x.checked_add(u64::from(n)))
                .filter(|&x| x <= max)
            {
                Some(x) => v = x,
                None => {
                    v = max;
                    over = true;
                }
            }
        }
    }
    (v, over, used)
}

/// `strtoul(.., 16)` on a 64-bit `unsigned long` (projectm-eval); saturates at `ULONG_MAX`.
fn strtoul16(digits: &[u8]) -> f64 {
    hex_prefix(digits, u64::MAX).0 as f64
}

/// Length of the ns-eel2 scanner's decimal literal at `i` (`[0-9]*\.[0-9]*([eE][0-9]*)?`, which
/// accepts a lone `.`), or 0.  An exponent is only recognised after a decimal point and takes
/// no sign: `1.5e3` is 1500, `1e3` is the number 1 and the name `e3`, `1.5e-3` is `1.5e` minus 3.
fn ns_decimal_len(b: &[u8], i: usize) -> usize {
    let mut j = i;
    while j < b.len() && is_digit(b[j]) {
        j += 1;
    }
    if j >= b.len() || b[j] != b'.' {
        return 0;
    }
    j += 1;
    while j < b.len() && is_digit(b[j]) {
        j += 1;
    }
    if j < b.len() && (b[j] == b'e' || b[j] == b'E') {
        j += 1;
        while j < b.len() && is_digit(b[j]) {
            j += 1;
        }
    }
    j - i
}

/// C `atof` of a literal that [`ns_decimal_len`] matched: the longest numeric prefix, so a bare
/// `.` and a dangling exponent (`1.5e`) parse as 0 and 1.5.
fn c_atof(text: &str) -> f64 {
    let t = text.trim_end_matches(['e', 'E']);
    if !t.bytes().any(|c| c.is_ascii_digit()) {
        return 0.0;
    }
    t.parse::<f64>().unwrap_or(0.0)
}

/// `atoi` on the digits of an integer literal: the MSVC CRT clamps at `INT_MAX`.
fn c_atoi(text: &str) -> f64 {
    match text.parse::<u64>() {
        Ok(v) => v.min(i32::MAX as u64) as f64,
        Err(_) => f64::from(i32::MAX),
    }
}

/// Is `name` a function the dialect lets programs call?
fn known_func(name: &str, ns: bool) -> bool {
    if ns {
        crate::tree::lookup_func_ns(name).is_some()
    } else {
        crate::tree::lookup_func(name).is_some()
    }
}

pub(crate) fn lex(src: &str, ns: bool) -> Vec<Token> {
    let mut b = src.as_bytes();
    // The C implementation scans a NUL terminated string.
    if let Some(p) = b.iter().position(|&c| c == 0) {
        b = &b[..p];
    }
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut line = 1u32;
    let mut line_start = 0usize;

    macro_rules! push {
        ($tok:expr, $start:expr, $len:expr) => {
            out.push(Token {
                tok: $tok,
                line,
                col: ($start - line_start) as u32,
                len: $len as u32,
            })
        };
    }

    while i < n {
        let c = b[i];
        match c {
            b' ' | b'\t' | b'\r' | 0x0b | 0x0c => {
                i += 1;
            }
            b'\n' => {
                i += 1;
                line += 1;
                line_start = i;
            }
            b'/' if i + 1 < n && b[i + 1] == b'*' => {
                i += 2;
                while i < n {
                    if b[i] == b'*' && i + 1 < n && b[i + 1] == b'/' {
                        i += 2;
                        break;
                    }
                    if b[i] == b'\n' {
                        line += 1;
                        line_start = i + 1;
                    }
                    i += 1;
                }
            }
            b'/' if i + 1 < n && b[i + 1] == b'/' => {
                i += 2;
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'$' if ns => {
                // ns-eel2 expands every `$` form in its source rewriter (`ns_pre`); one that is
                // still here is not a form it knows.
                push!(Tok::Undef(b'$'), i, 1);
                i += 1;
            }
            b'$' => {
                let start = i;
                let rest = &b[i + 1..];
                if rest.len() >= 2
                    && (rest[0] == b'x' || rest[0] == b'X')
                    && rest[1].is_ascii_hexdigit()
                {
                    let mut j = i + 2;
                    while j < n && b[j].is_ascii_hexdigit() {
                        j += 1;
                    }
                    push!(Tok::Num(strtoul16(&b[i + 2..j])), start, j - i);
                    i = j;
                } else if rest.len() >= 3
                    && rest[0] == b'\''
                    && rest[1] != b'\n'
                    && rest[2] == b'\''
                {
                    push!(Tok::Num(f64::from(rest[1] as i8)), start, 4);
                    i += 4;
                } else if rest.len() >= 3
                    && rest[0].eq_ignore_ascii_case(&b'p')
                    && rest[1].eq_ignore_ascii_case(&b'h')
                    && rest[2].eq_ignore_ascii_case(&b'i')
                {
                    // projectm-eval: `1.61803399f`, a float literal widened to double.
                    push!(Tok::Num(f64::from(f32::from_bits(0x3fcf_1bbd))), start, 4);
                    i += 4;
                } else if rest.len() >= 2
                    && rest[0].eq_ignore_ascii_case(&b'p')
                    && rest[1].eq_ignore_ascii_case(&b'i')
                {
                    push!(Tok::Num(f64::from(std::f32::consts::PI)), start, 3);
                    i += 3;
                } else if !rest.is_empty() && rest[0].eq_ignore_ascii_case(&b'e') {
                    push!(Tok::Num(f64::from(std::f32::consts::E)), start, 2);
                    i += 2;
                } else {
                    push!(Tok::Undef(b'$'), start, 1);
                    i += 1;
                }
            }
            c if ns && (is_digit(c) || c == b'.') => {
                // The scanner's DFA (`nseel-lextab.c`): the longest of `[0-9]*\.[0-9]*([eE][0-9]*)?`
                // (`atof`) and `[0-9]+` (`atoi`).
                let start = i;
                let dec = ns_decimal_len(b, i);
                let (v, len) = if dec > 0 {
                    (c_atof(&src[start..start + dec]), dec)
                } else {
                    let mut j = i;
                    while j < n && is_digit(b[j]) {
                        j += 1;
                    }
                    (c_atoi(&src[start..j]), j - start)
                };
                push!(Tok::Num(v), start, len);
                i += len;
            }
            c if is_digit(c) || c == b'.' => {
                let start = i;
                let mut j = i;
                while j < n && is_digit(b[j]) {
                    j += 1;
                }
                let int_digits = j - i;
                let mut end = j;
                if j < n && b[j] == b'.' {
                    let mut k = j + 1;
                    while k < n && is_digit(b[k]) {
                        k += 1;
                    }
                    let frac_digits = k - (j + 1);
                    if int_digits > 0 || frac_digits > 0 {
                        end = k + expo_len(b, k);
                    }
                } else if int_digits > 0 {
                    end = j + expo_len(b, j);
                }
                if end == start {
                    // lone '.'
                    push!(Tok::Undef(b'.'), start, 1);
                    i += 1;
                } else {
                    // Every numeric literal, integers included, is a C `atof`: projectM's vendored
                    // projectm-eval `Scanner.l` defines `FLOAT = ({DIGIT}+|...){EXPO}?`, which matches plain
                    // digit strings and precedes `INT`, so its `atoi` rule never fires (newer projectm-eval
                    // releases route them to `atoi`; the eel oracle patches that back, `docs/eel.md`).
                    // Large integers therefore lose precision instead of wrapping.
                    let v = src[start..end].parse::<f64>().unwrap_or(0.0);
                    push!(Tok::Num(v), start, end - start);
                    i = end;
                }
            }
            c if is_name_start(c) => {
                let start = i;
                let mut j = i + 1;
                // '.' is not an identifier character in either dialect: MD3's compiler rejects
                // `mouse.x` / `a.b` in preset code (docs/md3.md, measured), so the scanner ends the
                // name there and the parser fails on the stray number or '.'.
                while j < n && is_name_char(b[j]) {
                    j += 1;
                }
                let name = &src[start..j];
                let tok = if name.eq_ignore_ascii_case("gmem") && !ns {
                    // ns-eel2 has no `gmem` token: its rewriter turns `gmem[..]` into `_gmem(..)`
                    // and a bare `gmem` is an ordinary variable.
                    Tok::Gmem
                } else if known_func(name, ns) {
                    Tok::Func(name.to_string())
                } else {
                    Tok::Var(name.to_string())
                };
                push!(tok, start, j - start);
                i = j;
            }
            _ => {
                let start = i;
                let two = if i + 1 < n { Some(b[i + 1]) } else { None };
                let (tok, len) = match (c, two) {
                    (b'+', Some(b'=')) => (Tok::AddOp, 2),
                    (b'-', Some(b'=')) => (Tok::SubOp, 2),
                    (b'%', Some(b'=')) => (Tok::ModOp, 2),
                    (b'|', Some(b'=')) => (Tok::OrOp, 2),
                    (b'&', Some(b'=')) => (Tok::AndOp, 2),
                    (b'/', Some(b'=')) => (Tok::DivOp, 2),
                    (b'*', Some(b'=')) => (Tok::MulOp, 2),
                    (b'^', Some(b'=')) => (Tok::PowOp, 2),
                    (b'=', Some(b'=')) => (Tok::Equal, 2),
                    (b'<', Some(b'=')) => (Tok::BelEq, 2),
                    (b'>', Some(b'=')) => (Tok::AboEq, 2),
                    (b'!', Some(b'=')) => (Tok::NotEq, 2),
                    (b'|', Some(b'|')) => (Tok::BoolOr, 2),
                    (b'&', Some(b'&')) => (Tok::BoolAnd, 2),
                    (
                        b'<' | b'>' | b'+' | b'-' | b'*' | b'/' | b'%' | b'^' | b'&' | b'|' | b'!'
                        | b'=' | b'(' | b')' | b'[' | b']' | b'?' | b':' | b',' | b';',
                        _,
                    ) => (Tok::Ch(c), 1),
                    _ => (Tok::Undef(c), 1),
                };
                push!(tok, start, len);
                i += len;
            }
        }
    }
    out.push(Token {
        tok: Tok::Eof,
        line,
        col: (n - line_start.min(n)) as u32,
        len: 0,
    });
    out
}
