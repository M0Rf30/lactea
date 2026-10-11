//! HLSL tokenizer (produces the token stream the preprocessor and parser consume).

use crate::TranslateError;

#[derive(Clone, Debug, PartialEq)]
pub enum T {
    Ident(String),
    Int(i64),
    /// Float literal (value rounded to f32 on emission).
    Float(f64),
    Punct(&'static str),
}

#[derive(Clone, Debug)]
pub struct Tok {
    pub t: T,
    /// First token on its (logical) line.
    pub bol: bool,
    /// Whitespace directly before this token.
    pub space: bool,
    /// Macro names this token must not be re-expanded by (C hide set).
    pub hide: Vec<String>,
}

impl Tok {
    pub fn new(t: T) -> Tok {
        Tok {
            t,
            bol: false,
            space: true,
            hide: Vec::new(),
        }
    }
    pub fn is(&self, p: &str) -> bool {
        matches!(&self.t, T::Punct(q) if *q == p)
    }
    pub fn ident(&self) -> Option<&str> {
        match &self.t {
            T::Ident(s) => Some(s),
            _ => None,
        }
    }
}

const PUNCTS: &[&str] = &[
    "<<=", ">>=", "++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<", ">>", "<=",
    ">=", "==", "!=", "&&", "||", "##", "::", "+", "-", "*", "/", "%", "<", ">", "=", "!", "&",
    "|", "^", "~", "?", ":", ";", ",", ".", "(", ")", "[", "]", "{", "}", "#",
];

pub fn tokens_to_string(toks: &[Tok]) -> String {
    let mut s = String::new();
    for t in toks {
        if t.space && !s.is_empty() {
            s.push(' ');
        }
        match &t.t {
            T::Ident(i) => s.push_str(i),
            T::Int(i) => s.push_str(&i.to_string()),
            T::Float(f) => s.push_str(&format!("{f:?}")),
            T::Punct(p) => s.push_str(p),
        }
    }
    s
}

/// Tokenize. Comments must already be stripped (MD2 does that before compiling); stray
/// comments are tolerated anyway. Backslash-newline joins lines.
pub fn lex(src: &str) -> Result<Vec<Tok>, TranslateError> {
    let src = src.replace("\\\r\n", " ").replace("\\\n", " ");
    let b: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out: Vec<Tok> = Vec::new();
    let mut bol = true;
    let mut space = true;
    while i < b.len() {
        let c = b[i];
        if c == '\n' || c == '\r' || c == '\u{1}' {
            bol = true;
            space = true;
            i += 1;
            continue;
        }
        if c.is_whitespace() || c == '\u{0}' {
            space = true;
            i += 1;
            continue;
        }
        // comments (tolerated)
        if c == '/' && i + 1 < b.len() && b[i + 1] == '/' {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < b.len() && b[i + 1] == '*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            space = true;
            continue;
        }
        let t;
        if c.is_ascii_alphabetic() || c == '_' {
            let s = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == '_') {
                i += 1;
            }
            t = T::Ident(b[s..i].iter().collect());
        } else if c.is_ascii_digit() || (c == '.' && i + 1 < b.len() && b[i + 1].is_ascii_digit()) {
            let s = i;
            if c == '0' && i + 1 < b.len() && (b[i + 1] == 'x' || b[i + 1] == 'X') {
                i += 2;
                let h = i;
                while i < b.len() && b[i].is_ascii_hexdigit() {
                    i += 1;
                }
                let txt: String = b[h..i].iter().collect();
                let v = i64::from_str_radix(&txt, 16)
                    .map_err(|_| TranslateError::Lex(format!("bad hex literal 0x{txt}")))?;
                while i < b.len() && matches!(b[i], 'u' | 'U' | 'l' | 'L') {
                    i += 1;
                }
                t = T::Int(v);
            } else {
                let mut is_float = false;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                if i < b.len() && b[i] == '.' {
                    is_float = true;
                    i += 1;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                if i < b.len() && (b[i] == 'e' || b[i] == 'E') {
                    let mut j = i + 1;
                    if j < b.len() && (b[j] == '+' || b[j] == '-') {
                        j += 1;
                    }
                    if j < b.len() && b[j].is_ascii_digit() {
                        is_float = true;
                        while j < b.len() && b[j].is_ascii_digit() {
                            j += 1;
                        }
                        i = j;
                    }
                }
                let txt: String = b[s..i].iter().collect();
                if i < b.len() && matches!(b[i], 'f' | 'F' | 'h' | 'H') {
                    is_float = true;
                    i += 1;
                } else if i < b.len() && matches!(b[i], 'u' | 'U' | 'l' | 'L') && !is_float {
                    i += 1;
                }
                if is_float {
                    let v: f64 = txt
                        .parse()
                        .map_err(|_| TranslateError::Lex(format!("bad float literal {txt}")))?;
                    t = T::Float(v);
                } else {
                    // C-style octal is not supported by HLSL either; parse decimal.
                    let v: i64 = txt
                        .parse()
                        .map_err(|_| TranslateError::Lex(format!("bad int literal {txt}")))?;
                    t = T::Int(v);
                }
            }
        } else {
            let mut found = None;
            for p in PUNCTS {
                let pc: Vec<char> = p.chars().collect();
                if i + pc.len() <= b.len() && b[i..i + pc.len()] == pc[..] {
                    found = Some(*p);
                    break;
                }
            }
            match found {
                Some(p) => {
                    i += p.len();
                    t = T::Punct(p);
                }
                None => return Err(TranslateError::Lex(format!("unexpected character {c:?}"))),
            }
        }
        out.push(Tok {
            t,
            bol,
            space,
            hide: Vec::new(),
        });
        bol = false;
        space = false;
    }
    Ok(out)
}
