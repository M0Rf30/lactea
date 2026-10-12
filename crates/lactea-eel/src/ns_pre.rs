//! The ns-eel2 source rewriter (`preprocessCode` in `nseel-compiler.c`), the front half of
//! [`Dialect::NsEel2`](crate::Dialect::NsEel2) compilation.
//!
//! ns-eel2 does not parse most of its operators in the Bison grammar.  Before the grammar sees the
//! text, this pass strips comments, expands `$PI` / `$E` / `$PHI` / `$X..` / `$'c'`, turns a `;`
//! inside parentheses into the statement-sequencing `%`, and rewrites every `= += -= *= /= %= |= &=
//! ^= == != < > <= >= && || % ^ [ ] ! ?` into a call (`_set(a,b)`, `_if(c,a,b)`, `_mem(i)` ...)
//! by scanning left and right of the operator for its operands.  Operator precedence is therefore
//! a consequence of those scans, not of a precedence table: `^` and `%` take the single adjacent
//! operand on each side (one left-associative level), the comparisons and `&&` / `||` take
//! everything up to the nearest separator (`||` and `&&` share one left-associative level, below
//! the comparisons, and the arithmetic and bitwise operators sit inside their operands), and `=`
//! and `?:` take the whole rest of the expression.  What is left (`+ - * / & |`, unary `-`, the
//! sequencing `%`, calls, parentheses) is the grammar of `nseel-caltab.c`, parsed in `parser.rs`.
//!
//! The port keeps the scans byte for byte, including their quirks (see the comments), because the
//! rewritten text is what decides how a preset's expression evaluates.

// The recursion of the rewriter (the right-hand side of an operator is rewritten by a recursive
// call) is bounded by the caller's `Context::max_depth`; the C code has no limit and needs a deep
// native stack for adversarial input.

/// `isspace` in the C locale.
fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

fn is_alnum(c: u8) -> bool {
    c.is_ascii_alphanumeric()
}

/// Scan sets of the rewriter's `symbollists[]` (`""` means "the operand is one identifier or
/// parenthesised group").
const LISTS: [&[u8]; 6] = [b"", b":(,;?%", b",):?;", b",);", b",);", b""];

struct Sym {
    op: [u8; 2],
    lscan: i8,
    rscan: u8,
    func: &'static str,
}

const fn sym(a: u8, b: u8, lscan: i8, rscan: u8, func: &'static str) -> Sym {
    Sym {
        op: [a, b],
        lscan,
        rscan,
        func,
    }
}

/// `preprocSymbols[]`, in the scan order of the C code.
const SYMBOLS: [Sym; 22] = [
    sym(b'+', b'=', 0, 3, "_addop"),
    sym(b'-', b'=', 0, 3, "_subop"),
    sym(b'%', b'=', 0, 3, "_modop"),
    sym(b'|', b'=', 0, 3, "_orop"),
    sym(b'&', b'=', 0, 3, "_andop"),
    sym(b'/', b'=', 0, 3, "_divop"),
    sym(b'*', b'=', 0, 3, "_mulop"),
    sym(b'^', b'=', 0, 3, "_powop"),
    sym(b'=', b'=', 1, 2, "_equal"),
    sym(b'<', b'=', 1, 2, "_beleq"),
    sym(b'>', b'=', 1, 2, "_aboeq"),
    sym(b'<', 0, 1, 2, "_below"),
    sym(b'>', 0, 1, 2, "_above"),
    sym(b'!', b'=', 1, 2, "_noteq"),
    sym(b'|', b'|', 1, 2, "_or"),
    sym(b'&', b'&', 1, 2, "_and"),
    sym(b'=', 0, 0, 3, "_set"),
    sym(b'%', 0, 0, 0, "_mod"),
    sym(b'^', 0, 0, 0, "pow"),
    sym(b'[', 0, 0, 5, ""),
    sym(b'!', 0, -1, 0, ""),
    sym(b'?', 0, 1, 4, ""),
];
const SYM_BRACKET: usize = 19;
const SYM_NOT: usize = 20;
const SYM_QUESTION: usize = 21;

/// `strtoul(s, &end, 16)` on a 32-bit `unsigned long` (MSVC): returns the value, saturating at
/// `0xFFFFFFFF`, and the index just after the consumed text (`0` consumed when no digit follows).
fn strtoul16(s: &[u8]) -> (u32, usize) {
    let mut i = 0;
    while i < s.len() && is_space(s[i]) {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    if i + 2 < s.len()
        && s[i] == b'0'
        && (s[i + 1] == b'x' || s[i + 1] == b'X')
        && s[i + 2].is_ascii_hexdigit()
    {
        i += 2;
    }
    let first = i;
    let mut v: u64 = 0;
    let mut over = false;
    while i < s.len() && s[i].is_ascii_hexdigit() {
        let d = u64::from((s[i] as char).to_digit(16).unwrap_or(0));
        v = v * 16 + d;
        if v > u64::from(u32::MAX) {
            over = true;
            v = u64::from(u32::MAX);
        }
        i += 1;
    }
    if i == first {
        return (0, 0);
    }
    let v = if over {
        u32::MAX
    } else if neg {
        (v as u32).wrapping_neg()
    } else {
        v as u32
    };
    (v, i)
}

/// Rewrites `src` as `preprocessCode` does.  `Err` only when the nesting exceeds `max_depth`.
pub(crate) fn preprocess(src: &[u8], max_depth: u32) -> Result<Vec<u8>, ()> {
    let src = match src.iter().position(|&c| c == 0) {
        Some(n) => &src[..n],
        None => src,
    };
    rewrite(src, 0, max_depth)
}

fn rewrite(src: &[u8], depth: u32, max_depth: u32) -> Result<Vec<u8>, ()> {
    if depth > max_depth {
        return Err(());
    }
    // The C code reads a NUL terminated string; indices past the end read 0.
    let at = |i: usize| src.get(i).copied().unwrap_or(0);
    let mut buf: Vec<u8> = Vec::with_capacity(src.len() + 64);
    let mut p = 0usize;
    let mut semicnt = 0i32;

    while at(p) != 0 {
        if at(p) == b'/' {
            if at(p + 1) == b'/' {
                p += 2;
                while at(p) != 0 && at(p) != b'\n' {
                    p += 1;
                }
                continue;
            } else if at(p + 1) == b'*' {
                p += 2;
                while at(p) != 0 && (at(p) != b'*' || at(p + 1) != b'/') {
                    p += 1;
                }
                if at(p) != 0 {
                    p += 2;
                }
                continue;
            }
        }

        if at(p) == b'$' {
            let c1 = at(p + 1).to_ascii_uppercase();
            if c1 == b'X' {
                let (v, used) = strtoul16(&src[(p + 2).min(src.len())..]);
                p += 2 + used;
                buf.extend_from_slice(v.to_string().as_bytes());
                continue;
            }
            if at(p + 1) == b'\'' && at(p + 2) != 0 && at(p + 3) == b'\'' {
                buf.extend_from_slice(at(p + 2).to_string().as_bytes());
                p += 4;
                continue;
            }
            if c1 == b'P' && at(p + 2).eq_ignore_ascii_case(&b'I') {
                buf.extend_from_slice(b"3.141592653589793");
                p += 3;
                continue;
            }
            if c1 == b'E' {
                buf.extend_from_slice(b"2.71828183");
                p += 2;
                continue;
            }
            if c1 == b'P'
                && at(p + 2).eq_ignore_ascii_case(&b'H')
                && at(p + 3).eq_ignore_ascii_case(&b'I')
            {
                buf.extend_from_slice(b"1.61803399");
                p += 4;
                continue;
            }
        }

        let mut c = at(p);
        p += 1;
        if is_space(c) {
            c = b' ';
        }

        if c == b'(' {
            semicnt += 1;
        } else if c == b')' {
            semicnt = (semicnt - 1).max(0);
        } else if c == b';' && semicnt > 0 {
            // `;` inside parentheses: `%` (sequencing) when the next code character starts an
            // operand, otherwise a blank.
            let mut q = 0usize;
            let mut comment = 0u8;
            let mut nc;
            loop {
                nc = at(p + q);
                if nc == 0 {
                    break;
                }
                if comment == 0 && nc == b'/' {
                    if at(p + q + 1) == b'/' {
                        comment = 1;
                    } else if at(p + q + 1) == b'*' {
                        comment = 2;
                    }
                }
                if comment == 1 && nc == b'\n' {
                    comment = 0;
                } else if comment == 2 && nc == b'*' && at(p + q + 1) == b'/' {
                    q += 1;
                    comment = 0;
                } else if comment == 0 && !is_space(nc) {
                    break;
                }
                q += 1;
            }
            c = if nc != 0 && (is_alnum(nc) || matches!(nc, b'(' | b'_' | b'!' | b'$')) {
                b'%'
            } else {
                b' '
            };
        } else if !is_space(c) && !is_alnum(c) {
            let found = SYMBOLS
                .iter()
                .position(|s| c == s.op[0] && (s.op[1] == 0 || at(p) == s.op[1]));
            if let Some(n) = found {
                let sy = &SYMBOLS[n];

                // Left operand: scan back over what has been written so far.
                let l_text: Option<Vec<u8>> = if sy.lscan >= 0 {
                    let scan = LISTS[sy.lscan as usize];
                    let mut lsem = 0i32;
                    let mut lp = buf.len() as isize - 1;
                    while lp >= 0 {
                        let ch = buf[lp as usize];
                        if ch == b')' {
                            lsem += 1;
                            // A left operand nested deeper than the limit is rejected by the parser
                            // anyway; stopping here keeps long `^` / `%` chains linear.
                            if lsem > max_depth as i32 {
                                return Err(());
                            }
                        } else if ch == b'(' {
                            lsem -= 1;
                            if lsem < 0 {
                                break;
                            }
                        } else if lsem == 0 {
                            if scan.is_empty() {
                                if !is_space(ch) && !is_alnum(ch) && ch != b'_' && ch != b'.' {
                                    break;
                                }
                            } else if scan.contains(&ch) {
                                break;
                            }
                        }
                        lp -= 1;
                    }
                    let start = (lp + 1) as usize;
                    let text = buf.split_off(start);
                    Some(text)
                } else {
                    None
                };
                if sy.op[1] != 0 {
                    p += 1;
                }

                // Right operand: scan forward over the source.
                let r_begin = p;
                let mut rp = p;
                {
                    let rscan = sy.rscan;
                    let scan = LISTS[rscan as usize];
                    let mut rsem = 0i32;
                    let mut rq = 0i32;
                    let mut comment = 0u8;
                    let mut hashadch = false;
                    let mut bracket = 0i32;
                    while at(rp) != 0 {
                        let ch = at(rp);
                        if comment == 0 && ch == b'/' {
                            if at(rp + 1) == b'/' {
                                comment = 1;
                            } else if at(rp + 1) == b'*' {
                                comment = 2;
                            }
                        }
                        if comment == 1 && ch == b'\n' {
                            comment = 0;
                        } else if comment == 2 && ch == b'*' && at(rp + 1) == b'/' {
                            rp += 1;
                            comment = 0;
                        } else if comment == 0 {
                            if ch == b'(' {
                                hashadch = true;
                                rsem += 1;
                            } else if ch == b')' {
                                rsem -= 1;
                                if rsem < 0 {
                                    break;
                                }
                            } else if rsem == 0 {
                                if ch == b';' || ch == b',' {
                                    break;
                                }
                                if rscan == 0 {
                                    if ch == b':' {
                                        break;
                                    }
                                    if !is_space(ch)
                                        && !is_alnum(ch)
                                        && ch != b'_'
                                        && ch != b'.'
                                        && hashadch
                                    {
                                        break;
                                    }
                                    if is_alnum(ch) || ch == b'_' {
                                        hashadch = true;
                                    }
                                } else if rscan == 2
                                    && ((ch == b'|' && at(rp + 1) == b'|')
                                        || (ch == b'&' && at(rp + 1) == b'&'))
                                {
                                    break;
                                } else if rscan == 3 || rscan == 4 {
                                    if ch == b':' {
                                        rq -= 1;
                                    } else if ch == b'?' {
                                        rq += 1;
                                    }
                                    if rq < 3 - i32::from(rscan) {
                                        break;
                                    }
                                } else if rscan == 5 {
                                    if ch == b'[' {
                                        bracket += 1;
                                    } else if ch == b']' {
                                        bracket -= 1;
                                    }
                                    if bracket < 0 {
                                        break;
                                    }
                                }
                                if scan.contains(&ch) {
                                    break;
                                }
                            }
                        }
                        rp += 1;
                    }
                }
                let mut r_text = rewrite(&src[r_begin..rp.min(src.len())], depth + 1, max_depth)?;
                p = rp;

                match n {
                    SYM_BRACKET => {
                        let l = l_text.unwrap_or_default();
                        let lt = trim_start(&l);
                        let rt = trim_start(&r_text);
                        let is_gmem = lt.len() >= 4
                            && lt[..4].eq_ignore_ascii_case(b"gmem")
                            && (lt.len() == 4 || is_space(lt[4]));
                        if is_gmem {
                            buf.extend_from_slice(b"_gmem(");
                            if r_text.is_empty() {
                                buf.push(b'0');
                            } else {
                                buf.extend_from_slice(&r_text);
                            }
                        } else if !rt.is_empty() && rt != b"0" {
                            buf.extend_from_slice(b"_mem((");
                            buf.extend_from_slice(lt);
                            buf.extend_from_slice(b")+(");
                            buf.extend_from_slice(rt);
                            buf.push(b')');
                        } else {
                            buf.extend_from_slice(b"_mem(");
                            buf.extend_from_slice(lt);
                        }
                        if at(p) == b']' {
                            p += 1;
                        }
                    }
                    SYM_NOT => {
                        buf.extend_from_slice(b"_not(");
                        buf.extend_from_slice(&r_text);
                    }
                    SYM_QUESTION => {
                        // Split the branches at the `:` that balances the `?`.
                        let mut qcnt = 1i32;
                        let mut par = 0i32;
                        let mut i = 0usize;
                        while i < r_text.len() {
                            match r_text[i] {
                                b'?' => qcnt += 1,
                                b':' => qcnt -= 1,
                                b'(' => par += 1,
                                b')' => par -= 1,
                                _ => {}
                            }
                            if par < 0 {
                                break;
                            }
                            if par == 0 && qcnt == 0 && r_text[i] == b':' {
                                break;
                            }
                            i += 1;
                        }
                        let (t_part, f_part) = if i < r_text.len() {
                            let f = r_text.split_off(i + 1);
                            r_text.truncate(i);
                            (r_text, f)
                        } else {
                            (r_text, Vec::new())
                        };
                        let t_part = trim_start(&t_part);
                        let f_part = trim_start(&f_part);
                        buf.extend_from_slice(b"_if(");
                        buf.extend_from_slice(&l_text.unwrap_or_default());
                        buf.push(b',');
                        buf.extend_from_slice(if t_part.is_empty() { b"0" } else { t_part });
                        buf.push(b',');
                        buf.extend_from_slice(if f_part.is_empty() { b"0" } else { f_part });
                    }
                    _ => {
                        buf.extend_from_slice(sy.func.as_bytes());
                        buf.push(b'(');
                        buf.extend_from_slice(&l_text.unwrap_or_default());
                        buf.push(b',');
                        buf.extend_from_slice(&r_text);
                    }
                }
                c = b')';
            }
        }
        buf.push(c);
    }
    Ok(buf)
}

fn trim_start(s: &[u8]) -> &[u8] {
    let n = s.iter().take_while(|&&c| is_space(c)).count();
    &s[n..]
}

#[cfg(test)]
mod tests {
    use super::preprocess;

    fn pp(s: &str) -> String {
        String::from_utf8(preprocess(s.as_bytes(), 1000).unwrap()).unwrap()
    }

    #[test]
    fn rewrites_like_the_c_pass() {
        assert_eq!(pp("a=b;"), "_set(a,b);");
        assert_eq!(pp("a=b?c:d;"), "_set(a,_if(b,c,d));");
        assert_eq!(pp("x=a+b>c;"), "_set(x,_above(a+b,c));");
        assert_eq!(pp("a=2^3^2"), "_set(a,pow(pow(2,3),2))");
        assert_eq!(pp("-7%3"), "-_mod(7,3)");
        assert_eq!(pp("a||b&&c"), "_and(_or(a,b),c)");
        assert_eq!(pp("x[1]"), "_mem((x)+(1))");
        assert_eq!(pp("gmem[i]"), "_gmem(i)");
        assert_eq!(pp("if(a;b,c)"), "if(a%b,c)");
        assert_eq!(pp("$pi+$e"), "3.141592653589793+2.71828183");
        assert_eq!(pp("$XFF+$'A'"), "255+65");
    }
}
