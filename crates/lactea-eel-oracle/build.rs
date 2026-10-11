//! Builds projectm-eval (third_party/projectm-eval, tag recorded in PROJECTM_EVAL_VERSION)
//! with the `cc` crate.  The checked-in flex/bison output (Scanner.c / Compiler.c) is compiled
//! as is, so no flex/bison and no system install are needed.
//!
//! The C library keeps its `rand()` Mersenne Twister in function-local statics.  To make the
//! differential tests reproducible (and thread-safe) a *copy* of TreeFunctions.c is compiled in
//! which only changes the storage of that state: it becomes `__thread` and gets a reseed hook
//! (`prjm_oracle_seed_rng`).  The algorithm and the default seed are untouched.  A second
//! change makes `x % -1` return 0 instead of trapping for `INT64_MIN % -1` (C undefined
//! behaviour, SIGFPE on x86-64).  A copy of Scanner.c makes integer literals `atof` like
//! projectM's vendored lexer, see below.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

fn replace_once(src: &str, from: &str, to: &str) -> String {
    assert_eq!(
        src.matches(from).count(),
        1,
        "expected exactly one occurrence of {from:?} in TreeFunctions.c (projectm-eval version changed?)"
    );
    src.replacen(from, to, 1)
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let version = fs::read_to_string(manifest.join("PROJECTM_EVAL_VERSION"))
        .expect("PROJECTM_EVAL_VERSION")
        .trim()
        .to_string();
    let root = manifest.join("../../third_party/projectm-eval");

    if !root.join("projectm-eval/TreeFunctions.c").exists() {
        // third_party/projectm-eval is a gitignored clone: fetch it on demand.
        fs::create_dir_all(root.parent().unwrap())
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", root.parent().unwrap().display()));
        let status = Command::new("git")
            .args(["clone", "--quiet", "--branch", &version, "--depth", "1"])
            .arg("https://github.com/projectM-visualizer/projectm-eval")
            .arg(&root)
            .status()
            .expect("failed to run git");
        assert!(status.success(), "could not clone projectm-eval {version}");
    }
    let src = root.join("projectm-eval");

    // --- patched TreeFunctions.c ---
    let tf = fs::read_to_string(src.join("TreeFunctions.c")).expect("TreeFunctions.c");
    let tf = replace_once(
        &tf,
        "    static uint32_t mt[N]; /* the array for the state vector  */\n    static int32_t mti; /* mti==N+1 means mt[N] is not initialized */\n",
        "",
    );
    let tf = replace_once(
        &tf,
        "/* This is Milkdrop's original rand() implementation. */",
        "static __thread uint32_t mt[N];\nstatic __thread int32_t mti;\nstatic __thread uint32_t prjm_oracle_seed = 0x4141f00dUL;\n/* This is Milkdrop's original rand() implementation. */",
    );
    let tf = replace_once(
        &tf,
        "uint32_t s = 0x4141f00d; // Initial Mersenne Twister seed",
        "uint32_t s = prjm_oracle_seed; // Initial Mersenne Twister seed",
    );
    // `INT64_MIN % -1` traps (SIGFPE) in the C implementation (UB).  Make `% -1` yield 0, which
    // is the mathematically correct value and what every non-trapping input already produces.
    let tf = replace_once(
        &tf,
        "assign_ret_val((PRJM_EVAL_F) ((PRJM_EVAL_I) *val1_ptr % divisor));",
        "assign_ret_val((PRJM_EVAL_F) (divisor == -1 ? (PRJM_EVAL_I)0 : ((PRJM_EVAL_I) *val1_ptr % divisor)));",
    );
    let tf = replace_once(
        &tf,
        "assign_ret_val((PRJM_EVAL_F) ((PRJM_EVAL_I)(**ret_val) % divisor));",
        "assign_ret_val((PRJM_EVAL_F) (divisor == -1 ? (PRJM_EVAL_I)0 : ((PRJM_EVAL_I)(**ret_val) % divisor)));",
    );
    let mut tf = tf;
    tf.push_str(
        "\nvoid prjm_oracle_seed_rng(uint32_t s) { prjm_oracle_seed = s; mti = 0; }\n\
         uint32_t prjm_oracle_genrand(void) { return prjm_eval_genrand_int32(); }\n",
    );
    let patched = out.join("TreeFunctions_patched.c");
    fs::write(&patched, tf).unwrap();

    // --- patched Scanner.c ---
    // projectM's vendored projectm-eval lexes integer literals as `atof` (its `FLOAT` rule also
    // matches plain digit strings and precedes `INT`); the cloned release routes them through
    // `atoi`, which wraps (`3000000000` -> -1294967296).  The oracle must match projectM's
    // behaviour, so `INT` uses `atof` here.
    let sc = fs::read_to_string(src.join("Scanner.c")).expect("Scanner.c");
    let sc = replace_once(
        &sc,
        "yylval->NUM = (PRJM_EVAL_F)atoi(yytext);",
        "yylval->NUM = atof(yytext);",
    );
    let patched_scanner = out.join("Scanner_patched.c");
    fs::write(&patched_scanner, sc).unwrap();

    let mut b = cc::Build::new();
    b.include(&root)
        .include(&src)
        .include(src.join("api"))
        .define("PRJM_F_SIZE", "8")
        .opt_level(2)
        .warnings(false)
        .flag("-ffp-contract=off")
        .flag_if_supported("-fno-fast-math");
    // Benchmark-only: mimic projectM's default release config (-Ofast).  Never use for the
    // differential tests: -ffinite-math-only removes the isnan() guards.
    if env::var("LACTEA_ORACLE_OFAST").is_ok_and(|v| v == "1") {
        b.opt_level(3).flag("-Ofast");
    }
    println!("cargo:rerun-if-env-changed=LACTEA_ORACLE_OFAST");
    for f in [
        "Compiler.c",
        "CompileContext.c",
        "CompilerFunctions.c",
        "ExpressionTree.c",
        "MemoryBuffer.c",
        "TreeVariables.c",
        "api/projectm-eval.c",
    ] {
        b.file(src.join(f));
    }
    b.file(&patched);
    b.file(&patched_scanner);
    b.file(manifest.join("src/shim.c"));
    b.compile("projectm_eval");
    println!("cargo:rustc-link-lib=m");

    println!("cargo:rerun-if-changed=PROJECTM_EVAL_VERSION");
    println!("cargo:rerun-if-changed=src/shim.c");
    println!("cargo:rerun-if-changed=build.rs");
    for f in [
        "TreeFunctions.c",
        "Scanner.c",
        "Compiler.c",
        "MemoryBuffer.c",
    ] {
        println!(
            "cargo:rerun-if-changed={}",
            Path::new(&src).join(f).display()
        );
    }
}
