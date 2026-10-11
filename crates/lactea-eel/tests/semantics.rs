//! Semantic tests with hand-computed expectations (the oracle crate cross-checks the same
//! language against projectm-eval itself).

use lactea_eel::{Context, GlobalMemory, PROJECTM_DEFAULT_SEED};
use std::sync::Arc;

fn ctx() -> Context {
    Context::new(Arc::new(GlobalMemory::new()))
}

fn eval(src: &str) -> f64 {
    let mut c = ctx();
    let p = c.compile(src).unwrap_or_else(|e| panic!("{src}: {e}"));
    c.run(&p)
}

fn rejects(src: &str) {
    assert!(ctx().compile(src).is_err(), "should reject {src:?}");
}

#[test]
fn precedence_and_associativity() {
    assert_eq!(eval("2 + 3 * 4"), 14.0);
    assert_eq!(eval("2 ^ 3 ^ 2"), 64.0); // left associative: (2^3)^2
    assert_eq!(eval("-2 ^ 2"), 4.0); // unary minus binds tighter than ^
    assert_eq!(eval("!0 + 1"), 2.0);
    assert_eq!(eval("7 % 2 ^ 2"), 3.0); // % is above * but below ^: 7 % (2^2)
    assert_eq!(eval("5 * 3 % 2"), 5.0); // 5 * (3 % 2)
    assert_eq!(eval("1 ? 2 : 3 ? 4 : 5"), 2.0);
    assert_eq!(eval("0 ? 2 : 0 ? 4 : 5"), 5.0);
    assert_eq!(eval("a = b = 3; a + b"), 6.0);
    assert_eq!(eval("1 || 0 && 0"), 1.0);
    assert_eq!(eval("5 | 2 & 6"), 7.0);
}

#[test]
fn literals() {
    assert_eq!(eval("$xff"), 255.0);
    assert_eq!(eval("$'a'"), 97.0);
    assert_eq!(eval(".5 + 5. + 1e2 + 1.5E-1"), 0.5 + 5.0 + 100.0 + 0.15);
    // $pi/$e/$phi are float literals widened to double in projectm-eval.
    assert_eq!(eval("$pi"), f64::from(std::f32::consts::PI));
    assert_eq!(eval("$E"), f64::from(std::f32::consts::E));
    assert_eq!(eval("$Phi"), f64::from(f32::from_bits(0x3fcf_1bbd)));
    // integer literals are atof (projectm-eval 1.0.7's FLOAT rule matches plain digits): no int32 wrap-around
    assert_eq!(eval("3000000000"), 3_000_000_000.0);
    assert_eq!(eval("4294967296"), 4_294_967_296.0);
    assert_eq!(eval("0983624912364"), 983_624_912_364.0);
}

#[test]
fn division_modulo_and_epsilon_rules() {
    assert_eq!(eval("7 / 0"), 0.0);
    assert_eq!(eval("7 / 0.000001"), 0.0);
    assert_eq!(eval("7 % 0"), 0.0);
    assert_eq!(eval("7.9 % 3.9"), 1.0);
    assert_eq!(eval("-7 % 3"), -1.0);
    assert_eq!(eval("1 == 1.000001"), 1.0);
    assert_eq!(eval("1 != 1.000001"), 0.0);
    assert_eq!(eval("0.000001 && 1"), 0.0);
    assert_eq!(eval("0.000011 && 1"), 1.0);
    assert_eq!(eval("if(0.000001, 1, 2)"), 1.0); // if() uses exact != 0
    assert_eq!(eval("sqrt(-4)"), 2.0);
    assert_eq!(eval("log(-1) + log10(0)"), 0.0);
    assert_eq!(eval("pow(0, -1)"), 0.0);
    assert_eq!(eval("asin(2)"), 0.0);
    assert_eq!(eval("sign(-3) + sign(0) * 5"), -1.0);
}

#[test]
fn statement_lists_and_dead_code() {
    assert_eq!(eval("1; 2; 3"), 3.0);
    assert_eq!(eval("x = 1; x + 1;"), 2.0);
    assert_eq!(eval("x = 1;; ; y = 2"), 2.0);
    assert_eq!(eval("(1; 2) + 3"), 5.0);
    assert_eq!(eval(""), 0.0);
    assert_eq!(eval("// nothing\n/* at all */"), 0.0);
    assert_eq!(eval("x = 1; ()"), 1.0);
    rejects(";x = 1");
    rejects("x = 1 y = 2");
    rejects("x = ()");
}

#[test]
fn references_and_assignment() {
    assert_eq!(eval("a = 1; x = a + (a = 5); x"), 10.0); // operands are read after evaluation
    assert_eq!(eval("x = 3; x += (x = 7); x"), 14.0);
    assert_eq!(eval("if(1, a, b) = 9; a"), 9.0);
    assert_eq!(eval("a = 2; a ^= 3; a"), 8.0);
    assert_eq!(eval("a = 7; a %= 4; a"), 3.0);
    assert_eq!(eval("a = 6; a |= 3; a &= 5; a"), 5.0);
    assert_eq!(eval("x = 5; x /= 0; x"), 0.0);
}

#[test]
fn control_flow() {
    assert_eq!(eval("s = 0; loop(10, s += 2); s"), 20.0);
    assert_eq!(eval("s = 0; loop(2e6, s += 1); s"), 1_048_576.0);
    assert_eq!(eval("n = 5; f = 1; while(f *= n; n -= 1; n); f"), 120.0);
    assert_eq!(eval("exec2(a = 3, a + 1)"), 4.0);
    assert_eq!(eval("exec3(a = 3, b = 4, a * b)"), 12.0);
    assert_eq!(eval("1 && (a = 5); a"), 5.0);
    assert_eq!(eval("0 && (a = 5); a"), 0.0);
}

#[test]
fn memory() {
    assert_eq!(eval("megabuf(3) = 7; 3[]"), 7.0);
    assert_eq!(eval("10[5] = 3; 15[]"), 3.0);
    assert_eq!(eval("gmem[9] = 4; gmegabuf(9)"), 4.0);
    assert_eq!(eval("x = (megabuf(-2) = 5)"), 5.0); // out of range: writes into a scratch cell
    assert_eq!(eval("megabuf(-2) = 5; megabuf(-2)"), 0.0); // ... so reads give 0
    assert_eq!(eval("megabuf(-1) = 5; megabuf(0)"), 5.0); // (int)(-1 + 0.0001) == 0 in C
    assert_eq!(eval("megabuf(8388608) = 5; megabuf(8388608)"), 0.0);
    assert_eq!(eval("megabuf(8388607) = 5; megabuf(8388607)"), 5.0);
    assert_eq!(
        eval("memset(5, 3, 4); megabuf(4) * 100 + megabuf(5) * 10 + megabuf(8)"),
        33.0
    );
    assert_eq!(eval("memset(0, 2, 3); memcpy(10, 0, 3); megabuf(12)"), 2.0);
    assert_eq!(eval("megabuf(0.9999999) = 1; megabuf(1)"), 1.0); // index + 0.0001 rounds up
}

#[test]
fn globals_and_contexts() {
    let g = Arc::new(GlobalMemory::new());
    let mut a = Context::new(g.clone());
    let mut b = Context::new(g.clone());
    let pa = a
        .compile("reg07 = 3; gmem[1] = 4; megabuf(1) = 5;")
        .unwrap();
    let pb = b.compile("REG07 + gmem[1] + megabuf(1)").unwrap();
    a.run(&pa);
    assert_eq!(b.run(&pb), 7.0); // megabuf is per context
    assert_eq!(g.reg(7), 3.0);
    let v = a.var("reg07");
    assert_eq!(a.get(v), 3.0);
    assert!(a.var("reg7").reg_index().is_none()); // needs two digits
    assert!(a.var("reg07").reg_index() == Some(7));
}

#[test]
fn variables_are_case_insensitive_slots() {
    let mut c = ctx();
    let x = c.var("Foo");
    assert_eq!(c.var("fOO"), x);
    let p = c.compile("FOO += 2; foo *= 3").unwrap();
    c.set(x, 1.0);
    c.run(&p);
    assert_eq!(c.get(x), 9.0);
    let names: Vec<_> = c.vars().collect();
    assert_eq!(names, vec![("foo", 9.0)]);
    c.reset_vars();
    assert_eq!(c.get(x), 0.0);
}

#[test]
fn errors_carry_positions() {
    let mut c = ctx();
    let e = c.compile("x = 1;\ny = ) 2").unwrap_err();
    assert_eq!(e.line, 2);
    assert_eq!(e.column, 4);
    rejects("x = foo(1)");
    rejects("x = sin()");
    rejects("x = sin(1, 2)");
    rejects("min = 3");
    rejects("x = megabuf[1]");
    rejects("x = 1 @ 2");
    rejects("x = $y");
}

#[test]
fn rand_is_mt19937_with_projectm_seed() {
    // First outputs of MT19937 seeded with 1: 1791095845, 4282876139, ...
    let mut c = ctx();
    c.set_rng_seed(1);
    let p = c.compile("rand(2000000000)").unwrap();
    for exp in [1791095845u32, 4282876139, 3093770124, 4005303368] {
        let want = f64::from(exp) * (1.0 / 4294967295.0) * 2000000000.0;
        assert_eq!(c.run(&p).to_bits(), want.to_bits());
    }
    // reseeding reproduces the stream
    c.set_rng_seed(1);
    let first = c.run(&p);
    c.set_rng_seed(u64::from(PROJECTM_DEFAULT_SEED));
    let d1 = c.run(&p);
    c.set_rng_seed(1);
    assert_eq!(c.run(&p), first);
    c.set_rng_seed(u64::from(PROJECTM_DEFAULT_SEED));
    assert_eq!(c.run(&p), d1);
    // rand() statements without side effects are removed by the compiler (like projectm-eval)
    let mut c = ctx();
    let a = c.compile("rand(1); x = rand(100)").unwrap();
    let mut d = ctx();
    let b = d.compile("x = rand(100)").unwrap();
    assert_eq!(c.run(&a).to_bits(), d.run(&b).to_bits());
}

/// projectm-eval's generator is one process-wide stream: contexts of two presets (also two presets
/// blending during a soft cut) interleave on it, while their registers stay separate.
#[test]
fn rand_stream_can_be_shared_between_global_memories() {
    let owner = GlobalMemory::new();
    owner.set_rng_seed(1);
    let a = Arc::new(GlobalMemory::sharing_rng_with(&owner));
    let b = Arc::new(GlobalMemory::sharing_rng_with(&owner));
    let (mut ca, mut cb) = (Context::new(a.clone()), Context::new(b.clone()));
    let (pa, pb) = (
        ca.compile("x = rand(2000000000)").unwrap(),
        cb.compile("x = rand(2000000000)").unwrap(),
    );
    let got = [ca.run(&pa), cb.run(&pb), ca.run(&pa), cb.run(&pb)];
    for (g, exp) in got
        .iter()
        .zip([1791095845u32, 4282876139, 3093770124, 4005303368])
    {
        let want = f64::from(exp) * (1.0 / 4294967295.0) * 2000000000.0;
        assert_eq!(g.to_bits(), want.to_bits());
    }
    a.set_reg(3, 7.0);
    assert_eq!(b.reg(3), 0.0, "registers are per preset");
}

#[test]
fn programs_are_bound_to_their_context() {
    let g = Arc::new(GlobalMemory::new());
    let mut a = Context::new(g.clone());
    let mut b = Context::new(g);
    let p = a.compile("1").unwrap();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| b.run(&p)));
    assert!(r.is_err());
}

#[test]
fn deep_nesting_is_rejected_not_overflowed() {
    // The parser recurses up to `max_depth` (1000) levels; an unoptimised (debug) build needs more
    // than the 2 MiB default stack of a test thread for that, so run on an explicit 32 MiB one.
    std::thread::Builder::new()
        .stack_size(32 << 20)
        .spawn(|| {
            let src = format!("{}1{}", "(".repeat(100_000), ")".repeat(100_000));
            assert!(ctx().compile(&src).is_err());
            let chain = (0..1500).map(|_| "a").collect::<Vec<_>>().join("+");
            assert!(ctx().compile(&chain).is_err());
        })
        .unwrap()
        .join()
        .unwrap();
}
