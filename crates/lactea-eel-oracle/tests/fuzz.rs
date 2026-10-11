//! Random-program differential tests (proptest): structured programs exercising operators,
//! functions, nesting, loops and memory, plus token / literal soup for the scanner & parser.
//! `LACTEA_FUZZ_CASES=<n>` sets the proptest case count (default 3000).

use lactea_eel_oracle::diff::{CompileOutcome, Pair, Rng};
use proptest::prelude::*;

fn cases() -> u32 {
    match std::env::var("LACTEA_FUZZ_CASES") {
        Ok(s) => s
            .parse()
            .unwrap_or_else(|e| panic!("LACTEA_FUZZ_CASES={s:?} is not a case count: {e}")),
        Err(_) => 3000,
    }
}

/// Compile in both, require agreement, run `iters` iterations with random variable values.
fn differential(code: &str, iters: usize, seed: u64) -> Result<(), TestCaseError> {
    let mut p = Pair::new();
    match p.compile(code) {
        CompileOutcome::BothFailed { .. } => Ok(()),
        CompileOutcome::RustOnly(m) => Err(TestCaseError::fail(format!(
            "rust rejects, C accepts: {m}\n{code}"
        ))),
        CompileOutcome::COnly(m) => Err(TestCaseError::fail(format!(
            "C rejects, rust accepts: {m}\n{code}"
        ))),
        CompileOutcome::Both(prog) => {
            let mut rng = Rng::new(seed);
            for it in 0..iters {
                let names: Vec<String> = p.rust.vars().map(|(n, _)| n.to_string()).collect();
                for n in names {
                    let v = match rng.below(8) {
                        0 => 0.0,
                        1 => rng.range(-2.0, 2.0).round(),
                        2 => rng.range(-1e-4, 1e-4),
                        _ => rng.range(-5.0, 5.0),
                    };
                    p.set(&n, v);
                }
                let (r, c) = p.run(&prog);
                if let Err(m) = p.compare(r, c) {
                    return Err(TestCaseError::fail(format!("iter {it}: {m}\n{code}")));
                }
            }
            Ok(())
        }
    }
}

// ---------- structured generator ----------

const VARS: &[&str] = &["a", "b", "c", "d", "x", "y", "reg05", "q1"];
const NUMS: &[&str] = &[
    "0",
    "1",
    "2",
    "3",
    "5",
    "10",
    "0.5",
    ".25",
    "1.5e1",
    "100",
    "7.5",
    "0.00001",
    "0.000011",
    "1e-6",
    "1e30",
    "65536",
    "4294967296",
    "$pi",
    "$e",
    "$phi",
    "$xff",
    "$'a'",
    "1e300",
    "-0",
];
const UNARY_FN: &[&str] = &[
    "sin", "cos", "tan", "asin", "acos", "atan", "sqr", "sqrt", "exp", "log", "log10", "abs",
    "sign", "floor", "int", "ceil", "invsqrt", "bnot", "rand",
];
const BINARY_FN: &[&str] = &[
    "atan2", "pow", "min", "max", "sigmoid", "band", "bor", "equal", "above", "below", "exec2",
    "assign",
];
const BIN_OPS: &[&str] = &[
    "+", "-", "*", "/", "%", "^", "&", "|", "&&", "||", "==", "!=", "<", ">", "<=", ">=",
];
const ASSIGN_OPS: &[&str] = &["=", "+=", "-=", "*=", "/=", "%=", "^=", "|=", "&="];

#[derive(Debug, Clone)]
enum E {
    Num(&'static str),
    Var(&'static str),
    Un(&'static str, Box<E>),
    Bin(&'static str, Box<E>, Box<E>),
    Call1(&'static str, Box<E>),
    Call2(&'static str, Box<E>, Box<E>),
    If(Box<E>, Box<E>, Box<E>),
    Ternary(Box<E>, Box<E>, Box<E>),
    Exec3(Box<E>, Box<E>, Box<E>),
    Assign(&'static str, &'static str, Box<E>),
    MemAssign(&'static str, Box<E>, Box<E>),
    Mem(Box<E>, Option<Box<E>>),
    Gmem(Box<E>),
    List(Vec<E>),
    Loop(u8, Vec<E>),
    While(Vec<E>),
    /// `if(c, v1, v2) <op> rhs` : assignment through a reference returned by if()
    LvalIf(Box<E>, &'static str, &'static str, &'static str, Box<E>),
    /// `exec2(e, v) <op> rhs`
    LvalExec(Box<E>, &'static str, &'static str, Box<E>),
    /// `v + (v <op> rhs)` : operand read order
    Deferred(&'static str, &'static str, &'static str, Box<E>),
    /// `megabuf(i) <bin> (megabuf(i) = rhs)`
    DeferredMem(Box<E>, &'static str, Box<E>),
    /// `while((w -= 1) ? e : 0)` : while() whose condition mixes references and values
    WhileMixed(Box<E>),
    /// `(rvalue) <op> rhs`
    LvalRvalue(Box<E>, &'static str, Box<E>),
}

fn arb_e() -> impl Strategy<Value = E> {
    let leaf = prop_oneof![
        proptest::sample::select(NUMS).prop_map(E::Num),
        proptest::sample::select(VARS).prop_map(E::Var),
    ];
    leaf.prop_recursive(5, 48, 4, |inner| {
        let b = |s: BoxedStrategy<E>| s.prop_map(Box::new);
        let inner = inner.boxed();
        prop_oneof![
            (
                proptest::sample::select(&["-", "!", "+"][..]),
                b(inner.clone())
            )
                .prop_map(|(o, e)| E::Un(o, e)),
            (
                proptest::sample::select(BIN_OPS),
                b(inner.clone()),
                b(inner.clone())
            )
                .prop_map(|(o, l, r)| E::Bin(o, l, r)),
            (proptest::sample::select(UNARY_FN), b(inner.clone()))
                .prop_map(|(f, e)| E::Call1(f, e)),
            (
                proptest::sample::select(BINARY_FN),
                b(inner.clone()),
                b(inner.clone())
            )
                .prop_map(|(f, l, r)| E::Call2(f, l, r)),
            (b(inner.clone()), b(inner.clone()), b(inner.clone()))
                .prop_map(|(c, t, f)| E::If(c, t, f)),
            (b(inner.clone()), b(inner.clone()), b(inner.clone()))
                .prop_map(|(c, t, f)| E::Ternary(c, t, f)),
            (b(inner.clone()), b(inner.clone()), b(inner.clone()))
                .prop_map(|(c, t, f)| E::Exec3(c, t, f)),
            (
                proptest::sample::select(&VARS[..6]),
                proptest::sample::select(ASSIGN_OPS),
                b(inner.clone())
            )
                .prop_map(|(v, o, e)| E::Assign(v, o, e)),
            (
                proptest::sample::select(ASSIGN_OPS),
                b(inner.clone()),
                b(inner.clone())
            )
                .prop_map(|(o, i, e)| E::MemAssign(o, i, e)),
            (b(inner.clone()), proptest::option::of(b(inner.clone())))
                .prop_map(|(i, o)| E::Mem(i, o)),
            b(inner.clone()).prop_map(E::Gmem),
            proptest::collection::vec(inner.clone(), 1..4).prop_map(E::List),
            (0u8..6, proptest::collection::vec(inner.clone(), 1..3))
                .prop_map(|(n, v)| E::Loop(n, v)),
            proptest::collection::vec(inner.clone(), 1..3).prop_map(E::While),
            (
                b(inner.clone()),
                proptest::sample::select(&VARS[..6]),
                proptest::sample::select(&VARS[..6]),
                proptest::sample::select(ASSIGN_OPS),
                b(inner.clone())
            )
                .prop_map(|(c, v1, v2, o, e)| E::LvalIf(c, v1, v2, o, e)),
            (
                b(inner.clone()),
                proptest::sample::select(&VARS[..6]),
                proptest::sample::select(ASSIGN_OPS),
                b(inner.clone())
            )
                .prop_map(|(c, v, o, e)| E::LvalExec(c, v, o, e)),
            (
                proptest::sample::select(&VARS[..6]),
                proptest::sample::select(BIN_OPS),
                proptest::sample::select(ASSIGN_OPS),
                b(inner.clone())
            )
                .prop_map(|(v, bo, ao, e)| E::Deferred(v, bo, ao, e)),
            (
                b(inner.clone()),
                proptest::sample::select(BIN_OPS),
                b(inner.clone())
            )
                .prop_map(|(i, o, e)| E::DeferredMem(i, o, e)),
            b(inner.clone()).prop_map(E::WhileMixed),
            (
                b(inner.clone()),
                proptest::sample::select(ASSIGN_OPS),
                b(inner)
            )
                .prop_map(|(l, o, r)| E::LvalRvalue(l, o, r)),
        ]
    })
}

fn render(e: &E, out: &mut String, depth: usize) {
    // Parenthesise only sometimes, so that operator precedence is exercised too.
    let paren = |out: &mut String, f: &dyn Fn(&mut String)| {
        if depth % 3 == 1 {
            out.push('(');
            f(out);
            out.push(')');
        } else {
            f(out);
        }
    };
    match e {
        E::Num(n) => out.push_str(n),
        E::Var(v) => out.push_str(v),
        E::Un(o, e) => {
            out.push_str(o);
            render(e, out, depth + 1);
        }
        E::Bin(o, l, r) => paren(out, &|out| {
            render(l, out, depth + 1);
            out.push(' ');
            out.push_str(o);
            out.push(' ');
            render(r, out, depth + 1);
        }),
        E::Call1(f, e) => {
            out.push_str(f);
            out.push('(');
            render(e, out, depth + 1);
            out.push(')');
        }
        E::Call2(f, l, r) => {
            out.push_str(f);
            out.push('(');
            render(l, out, depth + 1);
            out.push_str(", ");
            render(r, out, depth + 1);
            out.push(')');
        }
        E::If(c, t, f) | E::Exec3(c, t, f) => {
            out.push_str(if matches!(e, E::If(..)) {
                "if("
            } else {
                "exec3("
            });
            render(c, out, depth + 1);
            out.push_str(", ");
            render(t, out, depth + 1);
            out.push_str(", ");
            render(f, out, depth + 1);
            out.push(')');
        }
        E::Ternary(c, t, f) => paren(out, &|out| {
            render(c, out, depth + 1);
            out.push_str(" ? ");
            render(t, out, depth + 1);
            out.push_str(" : ");
            render(f, out, depth + 1);
        }),
        E::Assign(v, o, e) => paren(out, &|out| {
            out.push_str(v);
            out.push(' ');
            out.push_str(o);
            out.push(' ');
            render(e, out, depth + 1);
        }),
        E::MemAssign(o, i, e) => paren(out, &|out| {
            out.push_str("megabuf(");
            render(i, out, depth + 1);
            out.push_str(") ");
            out.push_str(o);
            out.push(' ');
            render(e, out, depth + 1);
        }),
        E::Mem(i, off) => {
            out.push('(');
            render(i, out, depth + 1);
            out.push(')');
            out.push('[');
            if let Some(o) = off {
                render(o, out, depth + 1);
            }
            out.push(']');
        }
        E::Gmem(i) => {
            out.push_str("gmem[");
            render(i, out, depth + 1);
            out.push(']');
        }
        E::List(v) => {
            out.push('(');
            for (k, e) in v.iter().enumerate() {
                if k > 0 {
                    out.push_str("; ");
                }
                render(e, out, depth + 1);
            }
            out.push(')');
        }
        E::Loop(n, body) => {
            out.push_str(&format!("loop({n}, "));
            for (k, e) in body.iter().enumerate() {
                if k > 0 {
                    out.push_str("; ");
                }
                render(e, out, depth + 1);
            }
            out.push(')');
        }
        E::LvalIf(c, v1, v2, o, e) => {
            out.push_str("if(");
            render(c, out, depth + 1);
            out.push_str(&format!(", {v1}, {v2}) {o} "));
            render(e, out, depth + 1);
        }
        E::LvalExec(c, v, o, e) => {
            out.push_str("exec2(");
            render(c, out, depth + 1);
            out.push_str(&format!(", {v}) {o} "));
            render(e, out, depth + 1);
        }
        E::Deferred(v, bo, ao, e) => {
            out.push_str(&format!("({v} {bo} ({v} {ao} "));
            render(e, out, depth + 1);
            out.push_str("))");
        }
        E::DeferredMem(i, bo, e) => {
            out.push_str("(megabuf(");
            render(i, out, depth + 1);
            out.push_str(&format!(") {bo} (megabuf("));
            render(i, out, depth + 1);
            out.push_str(") = ");
            render(e, out, depth + 1);
            out.push_str("))");
        }
        E::WhileMixed(e) => {
            out.push_str(&format!("(wcnt{depth} = 3; while((wcnt{depth} -= 1) ? "));
            render(e, out, depth + 1);
            out.push_str(" : 0))");
        }
        E::LvalRvalue(l, o, r) => {
            out.push('(');
            render(l, out, depth + 1);
            out.push_str(") ");
            out.push_str(o);
            out.push(' ');
            render(r, out, depth + 1);
        }
        E::While(body) => {
            // bounded: a decrementing counter guards the loop (the interpreter also caps it).
            out.push_str(&format!("(wcnt{depth} = 3; while(wcnt{depth} -= 1; "));
            for e in body {
                render(e, out, depth + 1);
                out.push_str("; ");
            }
            out.push_str(&format!("wcnt{depth}))"));
        }
    }
}

fn arb_program() -> impl Strategy<Value = String> {
    proptest::collection::vec(arb_e(), 1..6).prop_map(|stmts| {
        let mut s = String::new();
        for (i, e) in stmts.iter().enumerate() {
            render(e, &mut s, 0);
            s.push_str(if i % 2 == 0 { ";\n" } else { "; " });
        }
        s
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), max_shrink_iters: 2000, ..ProptestConfig::default() })]

    #[test]
    fn structured_programs(code in arb_program(), seed in any::<u64>()) {
        differential(&code, 6, seed)?;
    }

    /// Token soup: random token sequences must be accepted/rejected identically and, when
    /// accepted, evaluate identically.
    #[test]
    fn token_soup(toks in proptest::collection::vec(proptest::sample::select(&[
        "a", "b", "x", "1", "2.5", ".5", "(", ")", "[", "]", ",", ";", "+", "-", "*", "/", "%", "^", "&", "|",
        "&&", "||", "!", "=", "==", "!=", "<", ">", "<=", ">=", "?", ":", "+=", "-=", "*=", "/=", "%=", "^=",
        "|=", "&=", "if", "sin", "max", "exec2", "loop", "while", "megabuf", "gmem", "$pi", "rand", "\n",
        "//c\n", "/*c*/", "reg01", "memset", "assign", "min", "band",
    ][..]), 1..16), seed in any::<u64>()) {
        let code = toks.join(" ");
        differential(&code, 3, seed)?;
    }

    /// Dense token soup without separating spaces: exercises longest-match scanning.
    #[test]
    fn char_soup(s in "[a-z0-9.eE+*/%^&|!=<>?:;,()\\[\\]$' _x-]{1,24}", seed in any::<u64>()) {
        differential(&s, 3, seed)?;
    }

    /// Numeric literal forms.
    #[test]
    fn number_literals(s in "(\\$[xX][0-9a-fA-F]{1,18}|[0-9]{0,12}\\.?[0-9]{0,6}([eE][+-]?[0-9]{1,4})?|\\$'.'|\\$(pi|PI|e|E|phi|Phi))") {
        let code = format!("x = {s}; y = -{s}");
        differential(&code, 1, 0)?;
    }
}
