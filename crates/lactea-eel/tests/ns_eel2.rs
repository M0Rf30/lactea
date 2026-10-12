//! `Dialect::NsEel2` (MilkDrop 3's expression engine): the behavioural table of the MD3 spec,
//! Annex A §7, plus the name / function-set rules of §2-§3.
//!
//! Each row runs `code` in a fresh context and reads one variable.  Rows marked "R-dep" depend on
//! the x87 rounding mode; the expected value is round-to-nearest-even, the dialect's documented
//! choice (docs/md3.md).

#![allow(clippy::excessive_precision)] // expected values are printed from the spec verbatim

use lactea_eel::{Context, Dialect, GlobalMemory};
use std::sync::Arc;

fn ns_mem() -> Arc<GlobalMemory> {
    Arc::new(GlobalMemory::with_dialect(Dialect::NsEel2))
}

fn ctx() -> Context {
    Context::new(ns_mem())
}

/// Runs `code` and returns the value of variable `var`.
fn var(code: &str, var: &str) -> f64 {
    let mut c = ctx();
    let p = c.compile(code).unwrap_or_else(|e| panic!("{code:?}: {e}"));
    c.run(&p);
    let v = c.var(var);
    c.get(v)
}

/// Runs `code` and returns the value of its last expression.
fn expr(code: &str) -> f64 {
    let mut c = ctx();
    let p = c.compile(code).unwrap_or_else(|e| panic!("{code:?}: {e}"));
    c.run(&p)
}

fn check(rows: &[(&str, f64)]) {
    for &(code, want) in rows {
        let got = var(code, "a");
        assert!(
            got == want || (got - want).abs() <= 1e-12 * want.abs().max(1.0),
            "{code:?}: got {got:?}, want {want:?}"
        );
    }
}

fn check_expr(rows: &[(&str, f64)]) {
    for &(code, want) in rows {
        let got = expr(code);
        assert!(got == want, "{code:?}: got {got:?}, want {want:?}");
    }
}

#[test]
fn arithmetic_and_sanitising() {
    check(&[
        ("a=7%3;", 1.0),
        ("a=7%0;", 0.0),
        ("a=-7%3;", -1.0), // the sign applies to the result (precedence)
        ("a=7%-3;", 1.0),
        ("a=10%3.5;", 2.0), // R-dep (truncation: 1)
        ("a=5%0.4;", 0.0),
        ("a=1/0;", 0.0),
        ("a=1/0.000001;", 1e6),
        ("a=sqrt(-4);", 2.0),
        ("a=sqr(-3);", 9.0),
        ("a=log(0);", 0.0),
        ("a=log(-1);", 0.0),
        ("a=log(100);", 4.605170185988092),
        ("a=log10(1000);", 3.0),
        ("a=exp(710);", 0.0),
        ("a=pow(2,10);", 1024.0),
        ("a=2^10;", 1024.0),
        ("a=(-8)^(1/3);", 0.0),
        ("a=pow(0,-1);", 0.0),
        ("a=asin(2);", 0.0),
        ("a=atan2(1,1);", 0.7853981633974483),
        ("a=pow(10,308)*10;", 0.0),
        ("a=pow(10,-310);", 0.0),
        ("a=int(-2.5);", -3.0),
        ("a=int(2.7);", 2.0),
        ("a=floor(-0.0001);", -1.0),
        ("a=ceil(-0.5);", 0.0),
        ("a=sign(0);", 0.0),
        ("a=sign(-4);", -1.0),
        ("a=sign(0.0000000001);", 1.0),
        ("a=abs(-3);", 3.0),
        ("a=min(2,3);", 2.0),
        ("a=max(2,3);", 3.0),
        ("a=sigmoid(1,1);", 0.7310585786300049),
        ("a=sigmoid(0,0);", 0.5),
        ("a=3.7|0;", 4.0), // R-dep (truncation: 3)
        ("a=6&3;", 2.0),
        ("a=6|3;", 7.0),
    ]);
}

#[test]
fn assignment_filters_but_compound_ops_do_not() {
    // `-0` is stored as +0
    let mut c = ctx();
    let p = c.compile("a=-0;").unwrap();
    c.run(&p);
    let a = c.var("a");
    assert_eq!(c.get(a).to_bits(), 0);

    // `a/=0` leaves +Inf; a plain copy filters it; the assignment's value is the stored value
    let mut c = ctx();
    let p = c
        .compile("a=1; a/=0; b=a; d=(a>1000000000); e=(f=1/0)+5;")
        .unwrap();
    c.run(&p);
    let (a, b, d, e, f) = (c.var("a"), c.var("b"), c.var("d"), c.var("e"), c.var("f"));
    assert_eq!(c.get(a), f64::INFINITY);
    assert_eq!(c.get(b), 0.0);
    assert_eq!(c.get(d), 1.0);
    assert_eq!(c.get(f), 0.0);
    assert_eq!(c.get(e), 5.0);

    // the other compound operators are raw too
    let mut c = ctx();
    let p = c
        .compile("a=0; a+=1/0; b=2; b^=1000000; d=5; d*=1/0;")
        .unwrap();
    c.run(&p);
    for n in ["a", "b", "d"] {
        let v = c.var(n);
        assert_eq!(c.get(v), f64::INFINITY, "{n}");
    }
}

#[test]
fn quirks_of_unary_helpers() {
    // sign keeps -0 until it is stored: a compound op sees it
    let mut c = ctx();
    let p = c.compile("a=1; a*=sign(-0);").unwrap();
    c.run(&p);
    let a = c.var("a");
    assert!(c.get(a).is_sign_negative() && c.get(a) == 0.0);

    // invsqrt: float32 magic number, not the exact value
    let inv4 = var("a=invsqrt(4);", "a");
    assert!((inv4 - 0.49915357).abs() < 1e-7, "{inv4}");
    assert!(inv4 != 0.5);
    let inv1 = var("a=invsqrt(1);", "a");
    assert!((inv1 - 0.99830715).abs() < 1e-7, "{inv1}");
}

#[test]
fn comparisons_and_logic() {
    check_expr(&[
        ("above(1,1)", 0.0),
        ("below(1,1)", 0.0),
        ("1>=1", 1.0),
        ("1<=1", 1.0),
        ("above(1,0.999999)", 1.0),
        ("equal(1,1.000001)", 1.0),
        ("equal(1,1.0001)", 0.0),
        ("bnot(0)", 1.0),
        ("bnot(0.000001)", 1.0),
        ("bnot(0.001)", 0.0),
        ("!5", 0.0),
        ("if(0.000001,10,20)", 20.0),
        ("if(0.001,10,20)", 10.0),
        ("0.00001&&1", 1.0),
        ("band(0.00001,1)", 0.0),
        ("bor(0.00001,0)", 0.0),
        ("band(0.00001,0.00001)", 0.0),
        ("exec2(1,2)", 2.0),
        ("exec3(1,2,3)", 3.0),
        ("(b=1; b+1)", 2.0),
        ("0.00001||0", 1.0),
        ("0.000001||0", 0.0),
        ("1&&2", 1.0),
    ]);
    assert_eq!(var("a=1; b=(a!=1.000001);", "b"), 0.0);
    assert_eq!(var("x=0; if(1,0,x=5);", "x"), 0.0);
    // short-circuit: `c` stays unset
    assert_eq!(var("a=1; b=(a||(c=5));", "b"), 1.0);
    assert_eq!(var("a=1; b=(a||(c=5));", "c"), 0.0);
    assert_eq!(var("a=0; b=(a&&(c=5));", "c"), 0.0);
}

#[test]
fn loops() {
    check(&[
        ("a=0; loop(3,a+=1);", 3.0),
        ("a=0; loop(0,a+=1);", 0.0),
        ("a=0; loop(-5,a+=1);", 0.0),
        ("a=0; loop(2000000,a+=1);", 1_048_576.0),
        ("a=0; loop(2.5,a+=1);", 2.0), // R-dep (truncation: 2)
        ("a=0; loop(2.6,a+=1);", 3.0), // R-dep (truncation: 2)
        ("a=0; while(exec2(a+=1,a<5));", 5.0),
        ("a=0; while(a+=1);", 1_048_576.0),
    ]);
}

#[test]
fn ram_model() {
    check(&[
        ("megabuf(0)=3; a=megabuf(0);", 3.0),
        ("megabuf(8388607)=5; a=megabuf(8388607);", 5.0),
        ("megabuf(8388608)=5; a=megabuf(8388608);", 0.0),
        ("a=megabuf(-1);", 0.0),                // R-dep (truncation: index 0)
        ("megabuf(1.6)=7; a=megabuf(2);", 7.0), // R-dep (truncation: 0)
        ("gmegabuf(0)=4; a=gmegabuf(1048576);", 4.0),
        ("gmegabuf(-1)=9; a=gmegabuf(1048575);", 9.0),
        ("memset(10,7,3); a=megabuf(11);", 7.0),
        (
            "megabuf(0)=1; megabuf(1)=2; memcpy(2,0,2); a=megabuf(3);",
            2.0,
        ),
        ("megabuf(4)=8; freembuf(0); a=megabuf(4);", 8.0),
        // bracket forms
        ("x[3]=6; a=x[3]+megabuf(3);", 12.0),
        ("gmem[1]=4; a=gmegabuf(1);", 4.0),
        // a bare `gmem` is an ordinary variable in ns-eel2
        ("gmem=3; a=gmem;", 3.0),
        // memcpy / memset truncate (no 0.0001 fudge)
        (
            "megabuf(0)=5; megabuf(1)=6; memcpy(2,0.9999,1); a=megabuf(2);",
            5.0,
        ),
        // the last valid address is 8388607, gmegabuf wraps over 1048576
        ("gmegabuf(1048577)=2; a=gmegabuf(1);", 2.0),
    ]);
}

#[test]
fn ram_sharing_between_contexts() {
    let g = ns_mem();
    let mut pf = Context::new(g.clone());
    let mut pv = Context::new(g.clone());
    let p1 = pf.compile("megabuf(5)=1; gmegabuf(5)=1; reg05=3;").unwrap();
    pf.run(&p1);
    let p2 = pv.compile("a=megabuf(5); b=gmegabuf(5); c=reg05;").unwrap();
    pv.run(&p2);
    let (a, b, c) = (pv.var("a"), pv.var("b"), pv.var("c"));
    assert_eq!(pv.get(a), 0.0, "megabuf is per context");
    assert_eq!(pv.get(b), 1.0, "gmegabuf is shared");
    assert_eq!(pv.get(c), 3.0, "regNN are shared");
}

#[test]
fn registers_and_gmegabuf_outlive_presets() {
    // MD3 keeps both in process-wide statics: a later preset (a memory derived from the engine's
    // owner) still sees them, in ns-eel2 only.
    let owner = GlobalMemory::with_dialect(Dialect::NsEel2);
    let first = Arc::new(GlobalMemory::sharing_rng_with(&owner));
    let mut c1 = Context::new(first);
    let p = c1.compile("reg07=11; gmegabuf(9)=12;").unwrap();
    c1.run(&p);
    let second = Arc::new(GlobalMemory::sharing_rng_with(&owner));
    assert_eq!(second.dialect(), Dialect::NsEel2);
    let mut c2 = Context::new(second);
    let p = c2.compile("a=reg07; b=gmegabuf(9);").unwrap();
    c2.run(&p);
    let (a, b) = (c2.var("a"), c2.var("b"));
    assert_eq!((c2.get(a), c2.get(b)), (11.0, 12.0));

    let owner = GlobalMemory::new();
    let first = Arc::new(GlobalMemory::sharing_rng_with(&owner));
    let mut c1 = Context::new(first);
    let p = c1.compile("reg07=11; gmegabuf(9)=12;").unwrap();
    c1.run(&p);
    let second = Arc::new(GlobalMemory::sharing_rng_with(&owner));
    let mut c2 = Context::new(second);
    let p = c2.compile("a=reg07; b=gmegabuf(9);").unwrap();
    c2.run(&p);
    let (a, b) = (c2.var("a"), c2.var("b"));
    assert_eq!(
        (c2.get(a), c2.get(b)),
        (0.0, 0.0),
        "projectm-eval: per preset"
    );
}

#[test]
fn rand_stream() {
    // fresh stream, MT19937 seeded with 0x4141f00d; draws are continuous in [0, max(1, floor(n))]
    let mut c = ctx();
    let p = c.compile("a=rand(1);").unwrap();
    let a = c.var("a");
    for want in [
        0.22772551216830628,
        0.3407441909286995,
        0.8936517685404168,
        0.949593038519284,
    ] {
        c.run(&p);
        assert!((c.get(a) - want).abs() < 1e-15, "{} vs {want}", c.get(a));
    }
    let mut c = ctx();
    assert!((var_in(&mut c, "a=rand(10);", "a") - 2.2772551216830628).abs() < 1e-14);
    for src in ["a=rand(0);", "a=rand(-3);", "a=rand(0.5);", "a=rand(1.9);"] {
        let mut c = ctx();
        assert!(
            (var_in(&mut c, src, "a") - 0.22772551216830628).abs() < 1e-15,
            "{src}"
        );
    }
    // raw outputs: 978073627, 1463485156, 3838205119, 4078471044.  A literal above 2147483647 is
    // clamped (`atoi`), so the generator word is scaled by the clamped limit.
    let mut c = ctx();
    let p = c.compile("a=rand(2147483647);").unwrap();
    let a = c.var("a");
    for raw in [978073627u32, 1463485156, 3838205119, 4078471044] {
        c.run(&p);
        let want = f64::from(raw) * (1.0 / 4294967295.0) * 2147483647.0;
        assert!((c.get(a) - want).abs() < 1e-6, "{} vs {want}", c.get(a));
    }
    // the run seed re-seeds the stream, reproducibly
    let mut c = ctx();
    c.set_rng_seed(7);
    let p = c.compile("a=rand(1000);").unwrap();
    c.run(&p);
    let a = c.var("a");
    let first = c.get(a);
    c.set_rng_seed(7);
    c.run(&p);
    assert_eq!(c.get(a), first);
}

fn var_in(c: &mut Context, code: &str, name: &str) -> f64 {
    let p = c.compile(code).unwrap();
    c.run(&p);
    let v = c.var(name);
    c.get(v)
}

#[test]
fn preprocessor_and_names() {
    check(&[
        ("a=$PI;", 3.141592653589793),
        ("a=$pi;", 3.141592653589793),
        ("a=$E;", 2.71828183),
        ("a=$PHI;", 1.61803399),
        ("a=$X10;", 16.0),
        ("a=$xff;", 255.0),
        // `$X` prints the (32-bit saturated) value as decimal text, which the scanner then reads
        // with `atoi`: above 2147483647 it clamps (measured: `$XFFFFFFFF / 1e9` is 2.147)
        ("a=$XFFFFFFFFFF;", 2147483647.0),
        ("a=$X7FFFFFFF;", 2147483647.0),
        ("a=$X80000000;", 2147483647.0),
        ("a=4294967295;", 2147483647.0),
        ("a=2147483647+1;", 2147483648.0),
        ("a=$'A';", 65.0),
        ("a=pi;", 0.0),
        ("a=e;", 0.0),
        ("a=phi;", 0.0),
        ("Foo=1; a=fOO;", 1.0),
        ("a=(b=2; c=3; b+c);", 5.0),
        ("a=(b=1; b+1);", 2.0),
        // the false branch of `?:` may be left out (it is 0)
        ("a=1?5;", 5.0),
        ("a=0?5;", 0.0),
        ("a=1?5:6;", 5.0),
        ("a=0?5:6;", 6.0),
    ]);
    assert!(
        Context::new(Arc::new(GlobalMemory::new()))
            .compile("a=1?5;")
            .is_err()
    );
    // `a=1` `b=2` joined without a separator is a syntax error (the whole part is dropped)
    let mut c = ctx();
    assert!(c.compile("a=1b=2").is_err());
    // comments end at the line break
    assert_eq!(var("a=1;//x\nb=2;", "b"), 2.0);
    assert_eq!(var("a=1;/* x */b=2;", "b"), 2.0);
}

/// Measured on MD3 3.37 (Wine): a '.' is not an identifier character, so `mouse.x` and `a.b` do not
/// compile in preset code (the part is dropped).
#[test]
fn dots_are_not_identifier_characters() {
    for src in ["a=mouse.x+2;", "a.b=3;", "a=b.c;", "mouse.x=1;"] {
        assert!(ctx().compile(src).is_err(), "{src}");
    }
    check(&[("a=1.5+.5;", 2.0), ("a=5.+1;", 6.0)]);
}

/// Measured on MD3 3.37 (Wine): the scanner has no exponent notation for integers (`1e3` is `1`
/// and the name `e3`), but a decimal literal accepts `[eE][0-9]*` after its point and no sign:
/// `1.5e1` is 15 and `1.0e-5` is `1.0e` minus 5 = -4 (`nseel-lextab.c` DFA, reproduced exactly).
#[test]
fn exponent_literals_follow_the_scanner_dfa() {
    for src in ["a=1e3;", "a=1e-5;", "a=1E5;"] {
        assert!(ctx().compile(src).is_err(), "{src}");
        assert!(
            Context::new(Arc::new(GlobalMemory::new()))
                .compile(src)
                .is_ok(),
            "{src} compiles under projectm-eval"
        );
    }
    check(&[
        ("a=1.5e1;", 15.0),  // measured
        ("a=1.0e-5;", -4.0), // measured: `1.0e` then `-5`
        ("a=3+.;", 3.0),     // measured: a lone `.` is the number 0
        ("a=2.5e3;", 2500.0),
        ("a=5.e2;", 500.0),
        ("a=.5E1;", 5.0),
        ("a=1.5e;", 1.5),   // a dangling `e` is part of the literal
        ("a=1.5e+2;", 3.5), // the sign is not part of the exponent
        ("a=.;", 0.0),
    ]);
    // an exponent is only recognised after the decimal point: `1e3` is `1` then the name `e3`
    assert!(ctx().compile("a=1 e3;").is_err());
}

/// Operator levels measured on MD3 3.37 (Wine): `%` and `^` are one left-associative level above
/// `*`; a unary minus covers a whole `%` / `^` chain but binds tighter than `&` and `|`.
#[test]
fn operator_precedence() {
    check(&[
        ("a=-7%3;", -1.0),
        ("b=-7; a=b%3;", 1.0),
        ("a=7%-3;", 1.0),
        ("a=-2^2;", -4.0),
        ("a=-2^2^2;", -16.0),
        ("a=2^3^2;", 64.0),
        ("a=2^-1*10;", 5.0),
        ("a=7%2^2;", 1.0),
        ("a=2^7%4;", 0.0),
        ("a=2*3^2;", 18.0),
        ("a=2*7%4;", 6.0),
        ("a=7%4*2;", 6.0),
        ("a=1+7%4;", 4.0),
        ("a=1+2^2;", 5.0),
        ("a=1-2%3;", -1.0),
        ("a=5&3|8;", 9.0),
        ("a=4+4&5;", 0.0),
        ("a=2*3&6;", 6.0),
        ("a=-5&3;", 3.0),
        ("a=3-2-1;", 0.0),
        ("a=!1+1;", 1.0),
    ]);
    // projectm-eval keeps its own levels: the sign binds tighter than `^`
    let mut c = Context::new(Arc::new(GlobalMemory::new()));
    let p = c.compile("a=-2^2;").unwrap();
    c.run(&p);
    let a = c.var("a");
    assert_eq!(c.get(a), 4.0);
}

/// The operators ns-eel2 rewrites in its source pass (`ns_pre`): every comparison, `&&` and `||`
/// takes whole operand texts (the arithmetic and bitwise operators bind inside them), and `&&` / `||`
/// share one left-associative level.  Rows marked "measured" are border pixels of MD3 3.37.
#[test]
fn rewriter_operator_levels() {
    check(&[
        ("a=2==3|1;", 0.0),   // measured: `2==(3|1)`
        ("a=1||0&&0;", 0.0),  // measured: `(1||0)&&0`
        ("a=2&1==0;", 1.0),   // measured: `(2&1)==0`
        ("a=(5; -6);", -1.0), // measured: `;` before a sign is a blank, so `5 -6`
        ("a=0||1&&0;", 0.0),
        ("a=0&&1||1;", 1.0),
        ("a=1+1==2;", 1.0),
        ("a=3>2==1;", 1.0),
        ("a=2==2>1;", 0.0), // `2==(2>1)`: a comparison takes everything up to `&&` / `||` / `?`
        ("a=3<2<1;", 0.0),  // `3<(2<1)`
        ("a=2==2==1;", 0.0), // `2==(2==1)`, not `(2==2)==1`
        ("a=1<2&&3>2;", 1.0),
        ("a=2^-1;", 0.5),
        ("a=4%3%2;", 1.0),
        ("a=(b=3)+1;", 4.0),
        ("a=(1; 2; 3);", 3.0),
        ("a=(1;2);", 2.0),
        ("a=5; b=a>3 ? 10 : 20; a=b;", 10.0),
        ("a=1?2:0?3:4;", 2.0),
        ("a=0?2:0?3:4;", 4.0),
        ("a=0?2:1?3:4;", 3.0),
        ("a=1; a+=1?2:3;", 3.0),
        ("a=0; 1 ? a=1 : a=2;", 1.0), // measured: compiles, assigns in the branch
        ("a=0; 0 ? a=1 : a=2;", 2.0),
        ("x=1; x[2]=7; a=megabuf(3);", 7.0), // `x[i]` is `megabuf(x+i)`
        ("gmem[5]=9; a=gmegabuf(5);", 9.0),
        ("a=gmem; gmem=4; a=gmem+a;", 4.0), // a bare `gmem` is an ordinary variable
        ("a=loop(3, b=b+1; b);", 3.0),
        ("a=if(1, 4; 5, 6);", 5.0),
    ]);
    // a statement the grammar rejects drops the whole text
    for src in [
        "a=1 2;", "a=(;", "a=]1;", "a=:1;", "a=?1;", "a=$Q;", "a=;;;)",
    ] {
        assert!(ctx().compile(src).is_err(), "{src}");
    }
}

/// Adversarial nesting is an error (rewriter recursion / tree depth), never a stack overflow.  The
/// compiler recurses up to its depth limit (1000); an unoptimised build needs more than the 2 MiB
/// of a default test thread for that, so this runs on a 32 MiB one.
#[test]
fn deep_nesting_is_rejected_not_overflowed() {
    std::thread::Builder::new()
        .stack_size(32 << 20)
        .spawn(|| {
            let n = 5_000;
            for src in [
                format!("a={}1;", "-".repeat(n)),
                format!("a={}1;", "!".repeat(n)),
                format!("{}1;", "a=".repeat(n)),
                format!("a={}1{};", "sin(".repeat(n), ")".repeat(n)),
                format!("a=b{};", "+b".repeat(n)),
                format!("a={}1{};", "(".repeat(n), ")".repeat(n)),
            ] {
                assert!(ctx().compile(&src).is_err(), "{}", &src[..20]);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

/// The rewriter honours `Context::set_max_depth`: a host that lowered the limit to fit a small
/// thread stack gets an error, not an abort.  Long `^` / `%` chains are rejected without
/// quadratic work.
#[test]
fn rewriter_honours_max_depth_on_a_small_stack() {
    std::thread::Builder::new()
        .stack_size(256 << 10)
        .spawn(|| {
            let n = 999;
            for src in [
                format!("r={}x;", "!".repeat(n)),
                format!("r={}x;", "a=".repeat(n)),
                format!("r=2{};", "^1".repeat(n)),
                format!("r=2{};", "%1".repeat(n)),
            ] {
                let mut c = ctx();
                c.set_max_depth(50);
                assert!(c.compile(&src).is_err(), "{}", &src[..12]);
            }
            let start = std::time::Instant::now();
            let mut c = ctx();
            c.set_max_depth(50);
            assert!(c.compile(&format!("r=2{};", "^1".repeat(40_000))).is_err());
            assert!(start.elapsed().as_secs() < 2, "long ^ chain is not linear");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn names_are_significant_up_to_128_bytes() {
    let long_a = format!("{}a", "n".repeat(128));
    let long_b = format!("{}b", "n".repeat(128));
    let code = format!("{long_a}=5; r={long_b};");
    assert_eq!(var(&code, "r"), 5.0);
    let short_a = format!("{}a", "n".repeat(127));
    let short_b = format!("{}b", "n".repeat(127));
    let code = format!("{short_a}=5; r={short_b};");
    assert_eq!(var(&code, "r"), 0.0);
}

/// The 53 entries of the ns-eel2 function table with their arities.
const TABLE: [(&str, usize); 53] = [
    ("_if", 3),
    ("_and", 2),
    ("_or", 2),
    ("loop", 2),
    ("while", 1),
    ("_not", 1),
    ("_equal", 2),
    ("_noteq", 2),
    ("_below", 2),
    ("_above", 2),
    ("_beleq", 2),
    ("_aboeq", 2),
    ("_set", 2),
    ("_mod", 2),
    ("_mulop", 2),
    ("_divop", 2),
    ("_orop", 2),
    ("_andop", 2),
    ("_addop", 2),
    ("_subop", 2),
    ("_modop", 2),
    ("sin", 1),
    ("cos", 1),
    ("tan", 1),
    ("asin", 1),
    ("acos", 1),
    ("atan", 1),
    ("atan2", 2),
    ("sqr", 1),
    ("sqrt", 1),
    ("pow", 2),
    ("_powop", 2),
    ("exp", 1),
    ("log", 1),
    ("log10", 1),
    ("abs", 1),
    ("min", 2),
    ("max", 2),
    ("sign", 1),
    ("rand", 1),
    ("floor", 1),
    ("ceil", 1),
    ("invsqrt", 1),
    ("sigmoid", 2),
    ("band", 2),
    ("bor", 2),
    ("exec2", 2),
    ("exec3", 3),
    ("_mem", 1),
    ("_gmem", 1),
    ("freembuf", 1),
    ("memcpy", 3),
    ("memset", 3),
];

#[test]
fn function_table_is_the_ns_eel2_one() {
    for (name, arity) in TABLE {
        let args = (0..arity)
            .map(|i| format!("x{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let mut c = ctx();
        // assignment targets (`_set(x0,x1)`) need variables; plain names are fine for all rows
        assert!(
            c.compile(&format!("{name}({args});")).is_ok(),
            "{name}/{arity} must compile"
        );
        // a wrong arity is an error
        let wrong = (0..=arity)
            .map(|i| format!("x{i}"))
            .collect::<Vec<_>>()
            .join(",");
        assert!(
            c.compile(&format!("{name}({wrong});")).is_err(),
            "{name} with {} args must not compile",
            arity + 1
        );
    }
    // aliases of the name table
    for src in [
        "a=if(1,2,3);",
        "a=bnot(0);",
        "assign(a,1);",
        "a=equal(1,1);",
        "a=below(1,2);",
        "a=above(2,1);",
        "a=megabuf(1);",
        "a=gmegabuf(1);",
        "a=int(1.5);",
    ] {
        assert!(ctx().compile(src).is_ok(), "{src}");
    }
    // `noteq`, `beleq`, `aboeq` are *not* aliased, only the underscore forms exist
    for src in ["a=noteq(1,2);", "a=beleq(1,2);", "a=aboeq(1,2);"] {
        assert!(ctx().compile(src).is_err(), "{src}");
    }
}

#[test]
fn projectm_eval_only_entries_are_not_callable() {
    // projectm-eval exposes the arithmetic operators as functions; ns-eel2's are JIT glue
    for src in [
        "a=_add(1,2);",
        "a=_sub(1,2);",
        "a=_mul(1,2);",
        "a=_div(1,2);",
        "a=_neg(1);",
    ] {
        assert!(ctx().compile(src).is_err(), "{src}");
        assert!(
            Context::new(Arc::new(GlobalMemory::new()))
                .compile(src)
                .is_ok(),
            "{src} stays callable in the default dialect"
        );
    }
    // functions of neither table
    for src in [
        "a=fmod(5,3);",
        "a=exp2(2);",
        "a=min3(1,2,3);",
        "a=sinh(1);",
        "a=gettime(0);",
    ] {
        assert!(ctx().compile(src).is_err(), "{src}");
    }
    // the operators themselves work
    check(&[
        ("a=3-1;", 2.0),
        ("a=-(2);", -2.0),
        ("a=+(2);", 2.0),
        ("a=6/3;", 2.0),
    ]);
}

#[test]
fn default_dialect_is_unchanged() {
    let mut c = Context::new(Arc::new(GlobalMemory::new()));
    assert_eq!(c.dialect(), Dialect::ProjectMEval);
    for (src, want) in [
        ("a=1/0;", 0.0),
        ("a=-7%3;", -1.0),
        ("a=sqrt(-4);", 2.0),
        ("a=3.7|0;", 3.0),
        ("a=log(0);", 0.0),
        ("a=pow(0,-1);", 0.0),
        ("a=megabuf(-1);", 0.0),
    ] {
        let mut c = Context::new(Arc::new(GlobalMemory::new()));
        let p = c.compile(src).unwrap();
        c.run(&p);
        let a = c.var("a");
        assert_eq!(c.get(a), want, "{src}");
    }
    // `a/=0` stays guarded, `$E` keeps its float32 value
    let p = c.compile("a=1; a/=0; b=$E;").unwrap();
    c.run(&p);
    let (a, b) = (c.var("a"), c.var("b"));
    assert_eq!(c.get(a), 0.0);
    assert_eq!(c.get(b), f64::from(std::f32::consts::E));
}

/// Text substitution of the `$` constants (`ns_pre`): `$E5` is the digits `2.718281835`.
#[test]
fn dollar_constants_are_text_substitutions() {
    check(&[
        ("a=$E5;", 2.718281835),
        ("a=$PI2;", 3.141592653589793_2),
        ("a=$PHI0;", 1.618033990),
        ("a=$X1F+$'a';", 31.0 + 97.0),
        ("a=$X 10;", 16.0),  // `strtoul` skips blanks
        ("a=$X0x10;", 16.0), // and accepts a `0x` prefix
        ("a=$X;", 0.0),      // no digits: the text `0`
    ]);
}

/// Every probe preset under `fixtures/md3/eel/` was run on the real MD3 3.37 (Wine, `md3shot.sh`,
/// 800x600, outer band at x = 20, inner band at x = 60): the bytes below are what it drew.  The
/// per-frame code is evaluated here without the renderer: a border channel is
/// `min(255, 255 * 2 * value)` (gamma 2), which MD3 draws within 2 of that (it is 126 for the
/// 0.25 of `t = 0`, not 127.5).  A channel that is `None` is not set by the probe.
#[test]
fn probe_fixtures_match_the_md3_screenshots() {
    const OB_IB: [&str; 6] = ["ob_r", "ob_g", "ob_b", "ib_r", "ib_g", "ib_b"];
    // (fixture, outer RGB, inner RGB); `None` = no per-frame code sets it
    type Shot = (&'static str, [Option<u8>; 3], [Option<u8>; 3]);
    let shots: [Shot; 18] = [
        (
            "colors",
            [Some(102), Some(204), Some(255)],
            [Some(204), Some(255), Some(254)],
        ),
        ("rounding", [Some(255); 3], [Some(255); 3]),
        (
            "mod_pow_sign",
            [Some(122), Some(132), Some(132)],
            [Some(106), Some(218), Some(158)],
        ),
        (
            "prec_mod_pow",
            [Some(146), Some(158), Some(158)],
            [Some(188), Some(192), Some(152)],
        ),
        (
            "prec_bitwise",
            [Some(172), Some(126), Some(158)],
            [Some(142), Some(126), Some(132)],
        ),
        (
            "literals",
            [Some(152), Some(142), Some(140)],
            [Some(122), Some(142), Some(160)],
        ),
        (
            "ternary_funcs",
            [Some(158), Some(162), Some(126)],
            [Some(126), Some(162), Some(192)],
        ),
        (
            "mod_zero_loops",
            [Some(152), Some(132), Some(136)],
            [Some(180), Some(180), Some(136)],
        ),
        (
            "truthiness",
            [Some(228), Some(152), Some(132)],
            [Some(132), Some(132), Some(126)],
        ),
        (
            "prec_unary",
            [Some(132), Some(126), Some(152)],
            [Some(106), Some(122), Some(44)],
        ),
        (
            "underscore_name",
            [Some(142), Some(132), Some(136)],
            [Some(142), Some(146), Some(152)],
        ),
        (
            "mod_negative",
            [Some(204), Some(255), Some(254)],
            [Some(255), Some(254), Some(255)],
        ),
        // `(5; -6)` is `5 -6`; `||` / `&&` share a level; a comparison takes `3|1` / `1` whole
        (
            "ns_logic",
            [Some(126), Some(126), Some(132)],
            [Some(122), None, None],
        ),
        // `1 ? t=4 : t=5` compiles and assigns in the branch
        ("ns_branch", [Some(146), None, None], [None, None, None]),
        // `1.5e1` is 15 (an exponent after the decimal point)
        ("ns_lit_exp", [Some(204), None, None], [None, None, None]),
        // `1.0e-5` is `1.0e` minus 5
        ("ns_lit_expneg", [Some(106), None, None], [None, None, None]),
        // a lone `.` is the number 0
        ("ns_lit_dot", [Some(142), None, None], [None, None, None]),
        // an integer literal and `$X..` are clamped to 2147483647 by `atoi`: 2.147 * 5.1 + 126
        (
            "ns_lit_big",
            [Some(138), Some(138), Some(138)],
            [None, None, None],
        ),
    ];
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/md3/eel");
    for (name, ob, ib) in shots {
        let text = std::fs::read_to_string(dir.join(format!("{name}.milk"))).unwrap();
        let mut lines: Vec<(u32, &str)> = text
            .lines()
            .filter_map(|l| {
                let rest = l.trim_end().strip_prefix("per_frame_")?;
                let (n, code) = rest.split_once('=')?;
                Some((n.parse().ok()?, code))
            })
            .collect();
        lines.sort_by_key(|l| l.0);
        let code: String = lines.iter().map(|l| l.1).collect();
        let mut c = ctx();
        let p = c.compile(&code).unwrap_or_else(|e| panic!("{name}: {e}"));
        c.run(&p);
        for (i, want) in ob.iter().chain(ib.iter()).enumerate() {
            let Some(want) = want else { continue };
            let v = c.var(OB_IB[i]);
            let got = (255.0 * 2.0 * c.get(v)).clamp(0.0, 255.0);
            assert!(
                (got - f64::from(*want)).abs() <= 3.0,
                "{name}: {} is {got:.1}, MD3 drew {want}",
                OB_IB[i]
            );
        }
    }
    // `mouse.x` does not compile: MD3 draws no border at all
    let text = std::fs::read_to_string(dir.join("mouse_dot_x.milk")).unwrap();
    let code: String = text
        .lines()
        .filter_map(|l| {
            l.trim_end()
                .strip_prefix("per_frame_")?
                .split_once('=')
                .map(|p| p.1)
        })
        .collect();
    assert!(ctx().compile(&code).is_err());
}
