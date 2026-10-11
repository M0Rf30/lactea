//! Hand-written differential cases: syntax corner cases, operator semantics, memory, rand.

use lactea_eel_oracle::diff::{CompileOutcome, Pair, Rng};

/// Returns Err(message) on any disagreement.
fn check(code: &str, iters: usize) -> Result<bool, String> {
    let mut p = Pair::new();
    match p.compile(code) {
        CompileOutcome::BothFailed { .. } => Ok(false),
        CompileOutcome::RustOnly(m) => Err(format!("rust rejects, C accepts: {code:?}: {m}")),
        CompileOutcome::COnly(m) => Err(format!("C rejects, rust accepts: {code:?}: {m}")),
        CompileOutcome::Both(prog) => {
            let mut rng = Rng::new(7);
            for it in 0..iters {
                // give every variable that exists a value
                let names: Vec<String> = p.rust.vars().map(|(n, _)| n.to_string()).collect();
                for n in names {
                    let v = if it == 0 { 0.0 } else { rng.range(-3.0, 3.0) };
                    p.set(&n, v);
                }
                let (r, c) = p.run(&prog);
                p.compare(r, c)
                    .map_err(|m| format!("{code:?} iter {it}: {m}"))?;
            }
            Ok(true)
        }
    }
}

#[test]
fn syntax_and_semantics() {
    let cases: &[&str] = &[
        // literals
        "x = 1; y = 1.5e3; z = .5; w = 5.; v = 1.e2; u = 2e-2; t = 007;",
        "x = $pi; y = $PI; z = $e; w = $E; v = $phi; u = $PHI;",
        "x = $xff; y = $XfF; z = $x7fffffffffffffffff; w = $'a'; v = $' ';",
        "x = 3000000000; y = 4294967296; z = 99999999999999999999;",
        "x = 1e400; y = 1e-400; z = 123456789012345678901234567890.5;",
        // comments / whitespace
        "x = 1; // comment\n y = 2; /* multi\nline */ z = 3;",
        "x = sin/*c*/(1);",
        "x = 1; /* unterminated",
        "x = 1; // end",
        "",
        "   \n\t ",
        "// only comment",
        // separators
        "x = 1; y = 2",
        "x = 1;; y = 2;;",
        ";x = 1",
        "x = 1; ()",
        "x = 1; (); y = 2",
        "x = (1; 2; 3)",
        "x = (a = 5; a + 1)",
        "(a = 1; b)",
        "x = 1 y = 2",
        // precedence
        "x = 2 + 3 * 4 - 1",
        "x = 2 ^ 3 ^ 2",
        "x = -2 ^ 2",
        "x = -a ^ 2",
        "x = !a ^ 2",
        "x = !a + 1",
        "x = - -3",
        "x = !!a",
        "x = a ? b : c ? d : e",
        "x = a ? b ? c : d : e",
        "x = a ? b = 1 : c = 2",
        "x = a ? b : c = 5",
        "x = 1 + a ? 2 : 3",
        "x = a || b && c",
        "x = a | b & c",
        "x = a == b != c",
        "x = a < b > c",
        "x = a <= b >= c",
        "x = 5 % 3 * 2",
        "x = 5 * 3 % 2",
        "x = 7 % 2 ^ 2",
        "x = a = b = 3",
        "x += y -= z *= 2",
        "x = a += 3",
        "x += a = 3",
        "x = 2 * a ? 1 : 0",
        "x = a ^ b * c",
        "x = !a * b",
        "x = !a[1]",
        "x = -a[1]",
        // assignment ops
        "x = 5; x += 3; x -= 1; x *= 4; x /= 3; x %= 4; x ^= 2; x |= 5; x &= 12;",
        "x = 5; x /= 0",
        "x = 5; x %= 0",
        "x = 0; x ^= -1",
        "x = -8; x ^= 0.5",
        "x = a + (a = 5)",
        "x = a; x += (x = 7)",
        "a = 1; x = a + (a = 5)",
        // functions
        "x = if(a, 1, 2); y = if(0, 1, 2); z = if(1, a = 3, b = 4);",
        "x = sin(a) + cos(a) + tan(a) + asin(a) + acos(a) + atan(a) + atan2(a, b)",
        "x = asin(5); y = acos(-5); z = log(-1); w = log10(0); v = log(10); u = log10(100)",
        "x = sqr(a) + sqrt(-4) + pow(a, 2) + pow(0, -1) + pow(-8, .5) + exp(1) + abs(-3)",
        "x = min(a, b) + max(a, b) + sign(a) + sign(0) + int(a * 5) + floor(-1.5) + ceil(-1.5)",
        "x = sigmoid(a, b); y = sigmoid(0, 0); z = invsqrt(a); w = invsqrt(0); v = invsqrt(-1)",
        "x = band(a, b) + bor(a, 0) + bnot(a) + bnot(0) + equal(a, a) + above(a, b) + below(a, b)",
        "x = 7 / 0; y = 7 % 0; z = 7.9 % 3.9; w = -7 % 3; v = 7 % -3; u = 1e30 % 7",
        "x = a / b; y = a / 0.000001; z = a / 0.00001",
        "x = a == b; y = a != b; z = a <= b; w = a >= b; v = a < b; u = a > b",
        "x = a == a + 0.000001; y = a != a + 0.000001; z = (a - a) == 0.00001",
        "x = a && b; y = a || b; z = !a; w = 0.000001 && 1; v = 0.000001 || 0",
        "x = (a && (b = 5)); y = (0 && (c = 5)); z = (1 || (d = 5)); w = (0 || (e = 5))",
        "x = exec2(a = 3, a + 1); y = exec3(a = 1, b = 2, a + b)",
        "x = exec3(a, b + 1, c)",
        "x = exec3(a = 4, 7, c); ",
        "loop(5, x += 1); y = loop(3, z += 2)",
        "loop(0, x += 1); y = loop(-1, x += 1); z = loop(1e9, w += 1)",
        "x = 5; while(x = x - 1)",
        "x = 0; while(x < 10 ? (x += 1; 1) : 0)",
        "x = 0; while(x += 1)",
        "x = 3; y = while(exec2(x -= 1, x))",
        "x = 3; while(if(x, y, z + 1)); w = if(1, x, y) = 5",
        "if(a, x, y) = 9",
        "(a, b)",
        "rand(1)",
        "x = rand(10); y = rand(0); z = rand(-5); w = rand(1.5); v = rand(100000)",
        "x = rand(5); rand(3); y = rand(5)",
        "x = rand(5); (rand(3); 1); y = rand(5)",
        "x = rand(5); if(a, rand(3), 1); y = rand(5)",
        "x = 1; rand(1); 2",
        // memory
        "megabuf(1) = 5; x = megabuf(1); y = megabuf(2); z = megabuf(1) + 3",
        "1[] = 7; x = 1[]; 10[5] = 3; y = 15[]; z = 14[1]; w = 0[15]",
        "gmem[3] = 9; x = gmem[3]; y = gmem[]; gmem[] = 4; z = gmem[0]",
        "gmegabuf(3) = 9; x = gmegabuf(3); y = _gmem(3)",
        "megabuf(-1) = 5; x = megabuf(-1); y = megabuf(1e9); megabuf(8388607) = 1; z = megabuf(8388607); w = megabuf(8388608)",
        "x = megabuf(0.99999); y = megabuf(a) ; megabuf(a) = 5; megabuf(1.00001) += 2",
        "i = 0; loop(100, megabuf(i) = i * 2; i += 1); x = megabuf(50) + megabuf(99)",
        "i = 0; loop(100, megabuf(i) = i * 2; i += 1); memcpy(200, 0, 100); x = megabuf(250)",
        "i = 0; loop(100, megabuf(i) = i; i += 1); memcpy(10, 0, 50); x = megabuf(40); y = megabuf(60)",
        "i = 0; loop(100, megabuf(i) = i; i += 1); memcpy(0, 10, 50); x = megabuf(40); y = megabuf(60)",
        "memset(5, 3, 10); x = megabuf(7); y = megabuf(15); z = megabuf(4)",
        "memset(65530, 3, 20); x = megabuf(65535); y = megabuf(65536); z = megabuf(65545)",
        "memcpy(65530, 0, 20); memset(0, 1, 30); memcpy(65530, 0, 20); x = megabuf(65540)",
        "memset(-5, 3, 10); x = megabuf(0); y = megabuf(4); z = megabuf(5)",
        "memcpy(-5, 0, 10); memcpy(0, -5, 10); x = megabuf(0)",
        "memset(8388600, 2, 100); x = megabuf(8388607)",
        "x = freembuf(5); y = freembuf(a)",
        "reg00 = 5; x = reg00; reg99 = 7; y = Reg99; z = reg5; reg123 = 4; w = reg100",
        "x = megabuf(1) = megabuf(2) = 3",
        "(a + b) = 3",
        "x = (a + b) += 3",
        "x = if(a, b, c) = 2; (a+1) = 5",
        "x = (y = 5) + 1; z = (y = 6) = 7",
        "x = _set(a, 3); y = _add(a, 1); z = assign(b, 2); w = _if(1, 2, 3); v = _mem(3)",
        "x = _and(1, 0); y = _or(1, 0); z = _not(0); w = _equal(1, 1); v = _neg(a)",
        "x = _addop(a, 1); y = _subop(a, 1); z = _mulop(a, 2); w = _divop(a, 2); v = _modop(a, 2); u = _powop(a, 2); t = _orop(a, 1); s = _andop(a, 1)",
        "x = 5 | 3 & 6; y = 5.9 | 3.9; z = -5 & 7; w = 1e30 | 1; v = -1e30 & 5",
        "x = 1e30 % 7; y = -1e30 % 7; z = 5 % 1e30; w = 5 % -1e30",
        "x = a % 1e300; y = 0 / 0; z = 1 / 0.000001",
        "x = 1 / (0 * 1e400)",
        "x = 1e400 - 1e400; y = x == x; z = x != x; w = x < 1; v = x > 1; u = min(x, 1); t = max(1, x)",
        "x = 1e400 - 1e400; y = if(x, 1, 2); z = !x; w = x && 1; v = x || 0; u = sign(x); t = x ^ 2; s = pow(x, 0)",
        "x = sqrt(1e400 - 1e400); y = exp(1000); z = exp(-1000); w = log(1e400); v = atan2(0, -1); u = atan2(-0, -1)",
        "x = 0; y = -x; z = -0; w = sign(-0); v = -0 + 0; u = 1 / (-0 + 0.000001)",
        "x = sin(1e22); y = cos(1e22); z = tan(1.5707963267948966); w = exp(709.78); v = pow(2, 1023.5); u = pow(10, -400)",
        "x = pow(-2, 3); y = pow(-2, 0.5); z = pow(0, 0); w = pow(0.000001, -1); v = pow(2, -0.5)",
        "x = int(1e30); y = int(-1e30); z = floor(0.5 - 1e-16)",
        // errors both must report
        "x = ",
        "x = 1 +",
        "x = (1",
        "x = 1)",
        "x = foo(1)",
        "x = sin()",
        "x = sin(1, 2)",
        "x = if(1, 2)",
        "x = min = 3",
        "x = gmem",
        "x = megabuf[1]",
        "x = 1 @ 2",
        "x = $",
        "x = $y",
        "x = $'ab'",
        "x = 1..2",
        "x = a b",
        "x = a (b)",
        "x = [1]",
        "x == ",
        "x = ()",
        "x = (;)",
        "x = (a;)",
        "x = sin(a;)",
        "x = sin(;a)",
        "x = max(a;b, c)",
        "x = max(a, ;c)",
        "x = {1}",
        "x = \"s\"",
        "x = a ? b",
        "x = a ? b :",
        "x = a : b",
        "x = 'a'",
        "x = 1e",
        "x = 1e+",
        "x = 1ee3",
        "x = 0x10",
        "x = 1_000",
        "X = Y; Z = x + y",
        "FooBar = 3; foobar += 1; FOOBAR *= 2",
        "x = loop(2)",
        "x = while(1, 2)",
        "x = exec2(1)",
        "x = memcpy(1, 2)",
        "x = gmem[1, 2]",
        "x = 3 3",
        "x = a ? : b",
        "x = a ?? b",
        "x = a,b",
        "x = ,",
    ];
    let mut compiled = 0;
    let mut errors = Vec::new();
    for c in cases {
        match check(c, 12) {
            Ok(true) => compiled += 1,
            Ok(false) => {}
            Err(e) => errors.push(e),
        }
    }
    eprintln!(
        "{} cases, {} compiled in both, {} mismatches",
        cases.len(),
        compiled,
        errors.len()
    );
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

#[test]
fn rand_stream_matches_c() {
    let mut p = Pair::new();
    let prog = match p.compile("x = rand(1000000); y = rand(7); z = rand(0.5);") {
        CompileOutcome::Both(c) => c,
        _ => panic!("compile"),
    };
    for _ in 0..5000 {
        let (r, c) = p.run(&prog);
        p.compare(r, c).unwrap();
    }
    // raw generator: first outputs of MT19937 seeded with 0x4141f00d
    lactea_eel_oracle::seed_rng(1);
    let c: Vec<u32> = (0..4).map(|_| lactea_eel_oracle::raw_genrand()).collect();
    // Standard MT19937 with seed 1 starts 1791095845, 4282876139, 3093770124, 4005303368.
    assert_eq!(c, vec![1791095845, 4282876139, 3093770124, 4005303368]);
    let mut q = Pair::new();
    q.reseed(1);
    let prog = match q.compile("x = rand(2000000000);") {
        CompileOutcome::Both(c) => c,
        _ => panic!("compile"),
    };
    for exp in [1791095845u32, 4282876139, 3093770124, 4005303368] {
        let (r, c) = q.run(&prog);
        q.compare(r, c).unwrap();
        let want = f64::from(exp) * (1.0 / 4294967295.0) * 2000000000.0;
        assert_eq!(r.to_bits(), want.to_bits(), "{r} vs {want}");
    }
}

#[test]
fn shared_globals_between_contexts() {
    let mut a = Pair::new();
    let mut b = Pair::sibling(&a);
    let pa = match a.compile("reg10 = 42; gmem[7] = 5;") {
        CompileOutcome::Both(c) => c,
        _ => panic!(),
    };
    let pb = match b.compile("x = reg10 + gmem[7]; y = megabuf(7);") {
        CompileOutcome::Both(c) => c,
        _ => panic!(),
    };
    let (r, c) = a.run(&pa);
    a.compare(r, c).unwrap();
    let (r, c) = b.run(&pb);
    b.compare(r, c).unwrap();
    let x = b.rust.var("x");
    assert_eq!(b.rust.get(x), 47.0);
}
