# lactea-eel notes

`lactea-eel` is a safe-Rust port of **projectm-eval v1.0.8** (MIT; tag recorded in
`crates/lactea-eel-oracle/PROJECTM_EVAL_VERSION`), the expression engine the projectM oracle runs.
`lactea-eel-oracle` builds the unmodified C sources with the `cc` crate (plus the two tiny
patches below) and differential-tests the port against them.

## Layout

| file | ports | notes |
|---|---|---|
| `lexer.rs` | `Scanner.l` | flex longest-match/first-rule semantics, 8-bit clean, NUL terminates input |
| `parser.rs` | `Compiler.y`, `CompilerFunctions.c` | precedence climbing with the exact Bison precedence table; same constant folding and dead-instruction elimination |
| `tree.rs` | `intrinsic_function_table` | function table with arity / const-eval / state-changing flags |
| `eval.rs` | `TreeFunctions.c` | exact interpreter: every function returns a *place* (`Loc`) like the C pointers |
| `fast.rs` | – | value-semantics tree used at run time; exotic constructs stay as `Slow(exact tree)` |
| `memory.rs` | `MemoryBuffer.c`, `TreeVariables.c` | megabuf (per context), gmegabuf + reg00..99 (shared, atomics) |
| `rng.rs` | `prjm_eval_genrand_int32` | MT19937 seeded `0x4141f00d` |

## Two evaluators

projectm-eval functions return pointers and write through the pointer they were handed, which
gives some surprising (but deterministic) behaviour that presets can in principle depend on
(`a + (a = 5)` reads `a` after the assignment, `exec3(a, b+1, c)` overwrites `a`, `if(c,a,b) = x`
assigns through the reference, a `while` condition keeps one out-pointer across iterations ...).
`eval.rs` reproduces this exactly.  After compilation the tree is lowered (`fast.rs`) into a
value-semantics tree for everything that provably behaves like plain values; the rest is wrapped
in `Slow` nodes and evaluated by `eval.rs`.  Both paths are covered by the differential tests.

## Seams for a future JIT

`Context::run` only needs `Program` + the context state (`vars`, megabuf, `GlobalMemory`).  A
cranelift/bytecode backend can lower the same exact tree (`tree::Node`) or the `fast::FNode` tree
and be selected inside `Context::compile`; no other API changes are needed.

## Known projectm-eval quirks reproduced on purpose

* `$pi`, `$e`, `$phi` are `float` literals widened to double (`3.1415927410125732`, `2.7182819843292236`, `1.6180340051651`).
* Integer literals are `atof` (the projectm-eval 1.0.7 that projectM vendors has `FLOAT = ({DIGIT}+|...){EXPO}?`, which also matches
  plain digit strings and precedes `INT`): `3000000000` stays `3000000000`. (v1.0.8 sends them through `atoi` and wraps to int32;
  the oracle projectM does not run it.) Probed through the oracle's `--vars` dump (`scripts/probes/ov_hash.milk`).
* `rand(x)` = `mt() * (1/0xFFFFFFFF) * max(floor(x),1)`; a `rand();` statement whose value is
  unused is removed by the compiler (not state changing) and does **not** advance the stream.
* `(int32)(x + 0.0001)` megabuf index: `megabuf(-1)` is cell 0; out-of-range cells are scratch.
* `%`: operands truncated to int64, `x % 0 == 0`; `/` by |d| < 1e-5 is 0; `&&`,`||`,`!`,`band`,
  `bor`, `while` use `|x| > 1e-5`; `if`/`?:` use exact `!= 0`.
* A leading `;` is a syntax error; `x = ()` is an error but `a; ()` is accepted.
* float -> int casts are x86-64 `cvttsd2si` (NaN / out of range -> `INT_MIN`).

## Platform dependence

Only the libm functions (`sin cos tan asin acos atan atan2 exp log log10 pow`) are platform
dependent: Rust's `f64` methods call the system libm (glibc on Linux), the same library the C
oracle links.  On targets without a system libm (wasm) results may differ in the last ulp.

## Oracle build patches (build.rs, copy of TreeFunctions.c only)

1. `mt[]`/`mti` become `__thread` + a reseed hook, so tests are reproducible and thread safe.
2. `x % -1` yields 0 instead of trapping on `INT64_MIN % -1` (C UB / SIGFPE).

projectm-eval's CMake default is `ENABLE_FAST_MATH=ON` (`-Ofast`, i.e. `-ffinite-math-only`, which
removes the `isnan()` guards).  For a deterministic oracle build projectM with fast-math off.
`LACTEA_ORACLE_OFAST=1` builds the oracle library that way for benchmarking only.
