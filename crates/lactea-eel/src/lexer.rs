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

/// `strtoul(.., 16)`: saturates at `ULONG_MAX`.
fn strtoul16(digits: &[u8]) -> f64 {
    let mut v: u64 = 0;
    for &d in digits {
        let n = (d as char)
            .to_digit(16)
            .expect("caller passes only ASCII hex digits") as u64;
        match v.checked_mul(16).and_then(|x| x.checked_add(n)) {
            Some(x) => v = x,
            None => return u64::MAX as f64,
        }
    }
    v as f64
}

pub(crate) fn lex(src: &str) -> Vec<Token> {
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
                    // `1.61803399f`: a float literal widened to double.
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
                    // Every numeric literal, integers included, is parsed as a C `atof` would
                    // (projectm-eval's `FLOAT` rule also matches plain digit strings), so large
                    // integers lose precision instead of wrapping like `atoi`.
                    let text = &src.as_bytes()[start..end];
                    let v = std::str::from_utf8(text)
                        .expect("literal scanned from ASCII digits, '.', 'e' and sign")
                        .parse::<f64>()
                        .unwrap_or(0.0);
                    push!(Tok::Num(v), start, end - start);
                    i = end;
                }
            }
            c if is_name_start(c) => {
                let start = i;
                let mut j = i + 1;
                while j < n && is_name_char(b[j]) {
                    j += 1;
                }
                let name = &src[start..j];
                let tok = if name.eq_ignore_ascii_case("gmem") {
                    Tok::Gmem
                } else if crate::tree::lookup_func(name).is_some() {
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
