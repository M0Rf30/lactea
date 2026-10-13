# lactea-eel notes

`lactea-eel` is a safe-Rust port of **projectm-eval v1.0.8** (MIT; tag recorded in
`crates/lactea-eel-oracle/PROJECTM_EVAL_VERSION`), the expression engine the projectM oracle runs.
`lactea-eel-oracle` builds the unmodified C sources with the `cc` crate (plus the small patches below) and differential-tests the port against them.

## Layout

| file | ports | notes |
|---|---|---|
| `lexer.rs` | `Scanner.l` | flex longest-match/first-rule semantics, 8-bit clean, NUL terminates input |
| `parser.rs` | `Compiler.y`, `CompilerFunctions.c` | precedence climbing with the exact Bison precedence table; same constant folding and dead-instruction elimination |
| `tree.rs` | `intrinsic_function_table` | function table with arity / const-eval / state-changing flags |
| `eval.rs` | `TreeFunctions.c` | exact interpreter: every function returns a *place* (`Loc`) like the C pointers |
| `fast.rs` | – | value-semantics tree used at run time; exotic constructs stay as `Slow(exact tree)` |
| `memory.rs` | `MemoryBuffer.c`, `TreeVariables.c` | megabuf (per context), gmegabuf + reg00..99 (shared, atomics); both are 128 blocks of 65536 doubles allocated on the first write: reads allocate nothing, and for megabuf neither does `memset` with 0 nor `memcpy` from an unallocated block (it zeroes an existing destination) |
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

## API contract (no panics on host input)

* `Context::try_run(&Program) -> Option<f64>` is `None` for a program compiled by another context; `run` returns 0 then.
* `Context::get` / `set` read 0 / ignore a `Var` that does not belong to the context; `GlobalMemory::reg` / `set_reg` read 0 / ignore `i >= 100`.
* Malformed source returns a `CompileError` (no `unwrap`/`expect` in the lexer, parser, evaluator or memory code). `tree::Node::List` is
  `{ s, init: Vec<Node>, last: Box<Node> }`, so a list always has its final expression by construction.

## Host variables the engine registers (not part of the evaluator)

`crates/lactea/src/vars.rs::register_host` adds BeatDrop's input variables to the **per-frame** and **per-vertex** contexts of a preset only when
`CompatRules::md2_input_vars` is on, which is `Compat::MilkDrop2` alone (projectM and MilkDrop 3: off, the names are then ordinary user variables that read 0; MD3's
mouse is a shader input and its grammar has no `mouse.x`):

* `mousex mousey` (`mousey` bottom-origin; both `-1` outside the window), `mousedown` (button level 0/1), `mouseclick` (1 for the one frame of the press edge),
  `ctrllt ctrlrt ctrldn ctrlup` (Ctrl + arrow keys held 0/1): per-frame and per-vertex. The per-vertex context gets copies of the per-frame values (`milkdropfs.cpp:798-805`).
* `year month day weekday hour minute second milliseconds totalseconds`: **per-frame only** (`state.cpp:349-365, 402-409`; `weekday` 1 = Monday .. 7 = Sunday).
* The engine does no I/O: the host sets them through `Engine::set_mouse`, `set_ctrl_keys` and `set_date_time` (until the host supplies a clock the date reads 0 and the
  time of day follows the frame clock, `totalseconds` = frame time mod 86400). They are sampled once per rendered frame (`md2_inputs.rs`) and shared by every state drawn
  that frame. Shape / wave code does not get them.
* `monitor` is an ordinary variable of the per-frame context (init and per-frame code read / write it, per-vertex code cannot see it; `Engine::monitor()` reads it back).
  It is treated as **persistent** in every compat mode: `milkdropfs.cpp:616` reloads `monitor_after_init_code` at the start of each frame, which by its name reads as "reset to
  the post-init value", but `:838` stores the per-frame result back into that member after the code ran, and `plugin.cpp:353-356` documents that a value written in one frame
  persists to the next; the name is the only ambiguity, the data flow persists.

## Known projectm-eval quirks reproduced on purpose

* `$pi`, `$e`, `$phi` are `float` literals widened to double (`3.1415927410125732`, `2.7182819843292236`, `1.6180340051651`).
* Integer literals are `atof`, so `3000000000` stays `3000000000` and `99999999999999999999` is `1e20`, no int32 wrap. projectM's
  vendored projectm-eval 1.0.7 has `FLOAT = ({DIGIT}+|...){EXPO}?`, which also matches plain digit strings and precedes `INT`;
  the cloned v1.0.8 `Scanner.c` sends them through `atoi` (wraps to int32), which the oracle projectM does not run, so the oracle build
  patches that line to `atof` (patch 3 below). The `handwritten` oracle test (`x = 3000000000; y = 4294967296; z = 99999999999999999999;`)
  and `tests/semantics.rs` pin the agreement; probed on the projectM side through the `--vars` dump (`scripts/probes/ov_hash.milk`).
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

## Oracle build patches (build.rs, copies of TreeFunctions.c and Scanner.c only)

1. `mt[]`/`mti` become `__thread` + a reseed hook, so tests are reproducible and thread safe.
2. `x % -1` yields 0 instead of trapping on `INT64_MIN % -1` (C UB / SIGFPE).
3. `Scanner.c` (a copy): `INT` literals use `atof` instead of `atoi`, matching projectM's vendored 1.0.7 scanner (see the quirks list).

projectm-eval's CMake default is `ENABLE_FAST_MATH=ON` (`-Ofast`, i.e. `-ffinite-math-only`, which
removes the `isnan()` guards).  For a deterministic oracle build projectM with fast-math off.
`LACTEA_ORACLE_OFAST=1` builds the oracle library that way for benchmarking only.
