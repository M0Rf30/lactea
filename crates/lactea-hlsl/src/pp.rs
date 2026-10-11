//! C preprocessor subset (what D3DX's preprocessor does for MilkDrop shaders):
//! object-like and function-like `#define`, `#undef`, `#if/#ifdef/#ifndef/#elif/#else/#endif`,
//! `#pragma`/`#line` (ignored), `#error`, `defined`, stringify-free `##` token pasting.

use crate::TranslateError;
use crate::lex::{T, Tok, lex};
use std::collections::{HashMap, VecDeque};

#[derive(Clone, Debug)]
struct Macro {
    params: Option<Vec<String>>,
    body: Vec<Tok>,
}

struct Cond {
    parent_active: bool,
    taken: bool,
    active: bool,
    seen_else: bool,
}

pub fn preprocess(src: &str) -> Result<Vec<Tok>, TranslateError> {
    let toks = lex(src)?;
    let mut macros: HashMap<String, Macro> = HashMap::new();
    let mut out: Vec<Tok> = Vec::new();
    let mut conds: Vec<Cond> = Vec::new();
    let mut i = 0;
    let mut pending: Vec<Tok> = Vec::new(); // non-directive tokens, expanded in chunks
    let flush = |pending: &mut Vec<Tok>,
                 macros: &HashMap<String, Macro>,
                 out: &mut Vec<Tok>|
     -> Result<(), TranslateError> {
        if !pending.is_empty() {
            let e = expand(std::mem::take(pending), macros)?;
            out.extend(e);
        }
        Ok(())
    };
    while i < toks.len() {
        let active = conds.last().map(|c| c.active).unwrap_or(true);
        if toks[i].bol && toks[i].is("#") {
            // collect directive line
            let mut j = i + 1;
            while j < toks.len() && !toks[j].bol {
                j += 1;
            }
            let line: Vec<Tok> = toks[i + 1..j].to_vec();
            i = j;
            let (name, rest) = match line.first() {
                Some(t) => match t.ident() {
                    Some(n) => (n.to_string(), line[1..].to_vec()),
                    None => (String::new(), line.clone()),
                },
                None => (String::new(), vec![]), // null directive
            };
            // Macro definitions affect expansion of preceding tokens only; flush first.
            flush(&mut pending, &macros, &mut out)?;
            match name.as_str() {
                "if" | "ifdef" | "ifndef" => {
                    let parent_active = active;
                    let v = if !parent_active {
                        false
                    } else if name == "if" {
                        eval_cond(&rest, &macros)? != 0
                    } else {
                        let n = rest.first().and_then(|t| t.ident()).unwrap_or("");
                        let d = macros.contains_key(n);
                        if name == "ifdef" { d } else { !d }
                    };
                    conds.push(Cond {
                        parent_active,
                        taken: v,
                        active: parent_active && v,
                        seen_else: false,
                    });
                }
                "elif" => {
                    let c = conds
                        .last_mut()
                        .ok_or_else(|| TranslateError::Preprocess("#elif without #if".into()))?;
                    if c.seen_else {
                        return Err(TranslateError::Preprocess("#elif after #else".into()));
                    }
                    if c.taken || !c.parent_active {
                        c.active = false;
                    } else {
                        let v = eval_cond(&rest, &macros)? != 0;
                        c.taken = v;
                        c.active = v;
                    }
                }
                "else" => {
                    let c = conds
                        .last_mut()
                        .ok_or_else(|| TranslateError::Preprocess("#else without #if".into()))?;
                    if c.seen_else {
                        return Err(TranslateError::Preprocess("duplicate #else".into()));
                    }
                    c.seen_else = true;
                    c.active = c.parent_active && !c.taken;
                    c.taken = true;
                }
                "endif" => {
                    conds
                        .pop()
                        .ok_or_else(|| TranslateError::Preprocess("#endif without #if".into()))?;
                }
                _ if !active => {}
                "define" => {
                    let nm = rest
                        .first()
                        .and_then(|t| t.ident())
                        .ok_or_else(|| TranslateError::Preprocess("#define without name".into()))?
                        .to_string();
                    let mut params = None;
                    let mut k = 1;
                    if rest.len() > 1 && rest[1].is("(") && !rest[1].space {
                        let mut ps = Vec::new();
                        k = 2;
                        loop {
                            match rest.get(k) {
                                Some(t) if t.is(")") => {
                                    k += 1;
                                    break;
                                }
                                Some(t) if t.is(",") => k += 1,
                                Some(t) if t.ident().is_some() => {
                                    ps.push(t.ident().unwrap_or_default().to_string());
                                    k += 1;
                                }
                                _ => {
                                    return Err(TranslateError::Preprocess(format!(
                                        "bad macro parameter list for {nm}"
                                    )));
                                }
                            }
                        }
                        params = Some(ps);
                    }
                    let mut body: Vec<Tok> = rest[k..].to_vec();
                    for b in &mut body {
                        b.bol = false;
                    }
                    if let Some(f) = body.first_mut() {
                        f.space = false;
                    }
                    macros.insert(nm, Macro { params, body });
                }
                "undef" => {
                    if let Some(n) = rest.first().and_then(|t| t.ident()) {
                        macros.remove(n);
                    }
                }
                "pragma" | "line" | "" => {}
                "error" => return Err(TranslateError::Preprocess("#error".into())),
                "include" => {
                    return Err(TranslateError::Preprocess(
                        "#include is not supported".into(),
                    ));
                }
                other => {
                    return Err(TranslateError::Preprocess(format!(
                        "unknown directive #{other}"
                    )));
                }
            }
            continue;
        }
        if active {
            pending.push(toks[i].clone());
        }
        i += 1;
    }
    flush(&mut pending, &macros, &mut out)?;
    if !conds.is_empty() {
        return Err(TranslateError::Preprocess("unterminated #if".into()));
    }
    Ok(out)
}

fn expand(input: Vec<Tok>, macros: &HashMap<String, Macro>) -> Result<Vec<Tok>, TranslateError> {
    let mut todo: VecDeque<Tok> = input.into();
    let mut out = Vec::new();
    let mut budget = 2_000_000usize;
    while let Some(tok) = todo.pop_front() {
        budget = budget
            .checked_sub(1)
            .ok_or_else(|| TranslateError::Preprocess("macro expansion too large".into()))?;
        let name = match tok.ident() {
            Some(n) if macros.contains_key(n) && !tok.hide.iter().any(|h| h == n) => n.to_string(),
            _ => {
                out.push(tok);
                continue;
            }
        };
        let m = &macros[&name];
        let mut hide = tok.hide.clone();
        hide.push(name.clone());
        match &m.params {
            None => {
                let mut rep: Vec<Tok> = m.body.clone();
                for (k, r) in rep.iter_mut().enumerate() {
                    r.hide.extend(hide.iter().cloned());
                    if k == 0 {
                        r.space = tok.space;
                    }
                }
                let rep = paste(rep)?;
                for r in rep.into_iter().rev() {
                    todo.push_front(r);
                }
            }
            Some(params) => {
                if !todo.front().map(|t| t.is("(")).unwrap_or(false) {
                    out.push(tok);
                    continue;
                }
                todo.pop_front();
                let mut args: Vec<Vec<Tok>> = vec![Vec::new()];
                let mut depth = 0;
                loop {
                    let t = todo.pop_front().ok_or_else(|| {
                        TranslateError::Preprocess(format!("unterminated call of macro {name}"))
                    })?;
                    if t.is("(") {
                        depth += 1;
                    } else if t.is(")") {
                        if depth == 0 {
                            break;
                        }
                        depth -= 1;
                    } else if t.is(",") && depth == 0 {
                        args.push(Vec::new());
                        continue;
                    }
                    if let Some(a) = args.last_mut() {
                        a.push(t);
                    }
                }
                if params.is_empty() && args.len() == 1 && args[0].is_empty() {
                    args.clear();
                }
                if args.len() != params.len() {
                    return Err(TranslateError::Preprocess(format!(
                        "macro {name} expects {} arguments, got {}",
                        params.len(),
                        args.len()
                    )));
                }
                let mut eargs = Vec::new();
                for a in args {
                    eargs.push(expand(a, macros)?);
                }
                let mut rep: Vec<Tok> = Vec::new();
                for b in &m.body {
                    if let Some(p) = b.ident().and_then(|n| params.iter().position(|q| q == n)) {
                        let mut a = eargs[p].clone();
                        if let Some(f) = a.first_mut() {
                            f.space = b.space;
                        }
                        rep.extend(a);
                    } else {
                        rep.push(b.clone());
                    }
                }
                for r in rep.iter_mut() {
                    r.hide.extend(hide.iter().cloned());
                }
                let rep = paste(rep)?;
                for r in rep.into_iter().rev() {
                    todo.push_front(r);
                }
            }
        }
    }
    Ok(out)
}

/// Resolve `a ## b` token pasting (only ident/number pastes). A paste whose result does not lex to at least
/// one token is an error (the operands are never silently dropped).
fn paste(toks: Vec<Tok>) -> Result<Vec<Tok>, TranslateError> {
    if !toks.iter().any(|t| t.is("##")) {
        return Ok(toks);
    }
    let mut out: Vec<Tok> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if toks[i].is("##")
            && i + 1 < toks.len()
            && let Some(l) = out.pop()
        {
            let r = &toks[i + 1];
            let s = format!(
                "{}{}",
                crate::lex::tokens_to_string(std::slice::from_ref(&l)),
                crate::lex::tokens_to_string(std::slice::from_ref(r))
            );
            let mut v = lex(&s).map_err(|_| {
                TranslateError::Preprocess(format!("pasting produced an invalid token: {s}"))
            })?;
            if v.is_empty() {
                return Err(TranslateError::Preprocess(format!(
                    "pasting produced no token: {s}"
                )));
            }
            let mut f = v.remove(0);
            f.space = l.space;
            f.hide = l.hide.clone();
            out.push(f);
            out.extend(v);
            i += 2;
        } else {
            out.push(toks[i].clone());
            i += 1;
        }
    }
    Ok(out)
}

fn eval_cond(rest: &[Tok], macros: &HashMap<String, Macro>) -> Result<i64, TranslateError> {
    // handle `defined X` / `defined(X)` before expansion
    let mut pre: Vec<Tok> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        if rest[i].ident() == Some("defined") {
            let (name, adv) = if rest.get(i + 1).map(|t| t.is("(")).unwrap_or(false) {
                (rest.get(i + 2).and_then(|t| t.ident()).unwrap_or(""), 4)
            } else {
                (rest.get(i + 1).and_then(|t| t.ident()).unwrap_or(""), 2)
            };
            let mut t = Tok::new(T::Int(macros.contains_key(name) as i64));
            t.space = true;
            pre.push(t);
            i += adv;
        } else {
            pre.push(rest[i].clone());
            i += 1;
        }
    }
    let e = expand(pre, macros)?;
    let mut p = CondParser { t: &e, i: 0 };
    let v = p.ternary()?;
    if p.i != e.len() {
        return Err(TranslateError::Preprocess(
            "trailing tokens in #if expression".into(),
        ));
    }
    Ok(v)
}

struct CondParser<'a> {
    t: &'a [Tok],
    i: usize,
}

impl CondParser<'_> {
    fn peek(&self) -> Option<&'static str> {
        match self.t.get(self.i) {
            Some(Tok { t: T::Punct(p), .. }) => Some(p),
            _ => None,
        }
    }
    fn ternary(&mut self) -> Result<i64, TranslateError> {
        let c = self.bin(0)?;
        if self.peek() == Some("?") {
            self.i += 1;
            let a = self.ternary()?;
            if self.peek() != Some(":") {
                return Err(TranslateError::Preprocess("expected : in #if".into()));
            }
            self.i += 1;
            let b = self.ternary()?;
            return Ok(if c != 0 { a } else { b });
        }
        Ok(c)
    }
    fn bin(&mut self, min: u8) -> Result<i64, TranslateError> {
        let mut l = self.unary()?;
        while let Some(op) = self.peek() {
            let prec = match op {
                "||" => 1,
                "&&" => 2,
                "|" => 3,
                "^" => 4,
                "&" => 5,
                "==" | "!=" => 6,
                "<" | ">" | "<=" | ">=" => 7,
                "<<" | ">>" => 8,
                "+" | "-" => 9,
                "*" | "/" | "%" => 10,
                _ => break,
            };
            if prec < min.max(1) {
                break;
            }
            self.i += 1;
            let r = self.bin(prec + 1)?;
            l = match op {
                "||" => ((l != 0) || (r != 0)) as i64,
                "&&" => ((l != 0) && (r != 0)) as i64,
                "|" => l | r,
                "^" => l ^ r,
                "&" => l & r,
                "==" => (l == r) as i64,
                "!=" => (l != r) as i64,
                "<" => (l < r) as i64,
                ">" => (l > r) as i64,
                "<=" => (l <= r) as i64,
                ">=" => (l >= r) as i64,
                "<<" => l.wrapping_shl(r as u32),
                ">>" => l.wrapping_shr(r as u32),
                "+" => l.wrapping_add(r),
                "-" => l.wrapping_sub(r),
                "*" => l.wrapping_mul(r),
                "/" => l
                    .checked_div(r)
                    .ok_or_else(|| TranslateError::Preprocess("division error in #if".into()))?,
                "%" => l
                    .checked_rem(r)
                    .ok_or_else(|| TranslateError::Preprocess("division error in #if".into()))?,
                _ => {
                    return Err(TranslateError::Preprocess(format!(
                        "unsupported operator {op} in #if"
                    )));
                }
            };
        }
        Ok(l)
    }
    fn unary(&mut self) -> Result<i64, TranslateError> {
        match self.t.get(self.i) {
            Some(Tok { t: T::Punct(p), .. }) => {
                let p = *p;
                self.i += 1;
                match p {
                    "!" => Ok((self.unary()? == 0) as i64),
                    "-" => self.unary()?.checked_neg().ok_or_else(|| {
                        TranslateError::Preprocess("integer overflow in #if".into())
                    }),
                    "+" => self.unary(),
                    "~" => Ok(!self.unary()?),
                    "(" => {
                        let v = self.ternary()?;
                        if self.peek() != Some(")") {
                            return Err(TranslateError::Preprocess("expected ) in #if".into()));
                        }
                        self.i += 1;
                        Ok(v)
                    }
                    _ => Err(TranslateError::Preprocess(format!("unexpected {p} in #if"))),
                }
            }
            Some(Tok { t: T::Int(v), .. }) => {
                self.i += 1;
                Ok(*v)
            }
            Some(Tok { t: T::Float(v), .. }) => {
                self.i += 1;
                Ok(*v as i64)
            }
            Some(Tok { t: T::Ident(_), .. }) => {
                self.i += 1;
                Ok(0)
            }
            None => Err(TranslateError::Preprocess(
                "unexpected end of #if expression".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(src: &str) -> String {
        match preprocess(src) {
            Ok(_) => panic!("expected an error for {src:?}"),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn conditional_directive_order_is_checked() {
        assert!(err("#if 1\n#else\n#else\n#endif\n").contains("duplicate #else"));
        assert!(err("#if 1\n#else\n#elif 1\n#endif\n").contains("#elif after #else"));
        assert!(preprocess("#if 0\n#elif 1\nx\n#else\ny\n#endif\n").is_ok());
    }

    #[test]
    fn if_expression_overflow_is_an_error() {
        assert!(err("#if -(-9223372036854775807 - 1)\n#endif\n").contains("overflow"));
        assert!(err("#if 1 / 0\n#endif\n").contains("division"));
    }

    #[test]
    fn invalid_paste_is_an_error() {
        assert!(err("#define P(a, b) a ## b\nP(0, x)\n").contains("pasting"));
    }
}
