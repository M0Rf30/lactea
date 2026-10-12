//! ns-eel2 / projectm-eval compatible expression language (MilkDrop per-frame/per-vertex code).
//!
//! The semantics are a port of projectm-eval v1.0.8 (MIT), which is what the projectM oracle
//! runs: scanner (`Scanner.l`), grammar and operator precedence (`Compiler.y`), constant folding
//! and dead-instruction elimination (`CompilerFunctions.c`), function implementations
//! (`TreeFunctions.c`), memory (`MemoryBuffer.c`) and variables (`TreeVariables.c`).
//!
//! * All arithmetic is `f64`; transcendental functions call the platform libm through
//!   `f64::{sin,cos,tan,asin,acos,atan,atan2,exp,ln,log10,powf}` (glibc on Linux, which is what the
//!   C oracle links as well).  These are the only platform-dependent operations.
//! * Float -> integer conversions follow x86-64 `cvttsd2si` (out of range / NaN -> `INT_MIN`),
//!   which is what the C code compiles to.
//! * No `unsafe`, no threads, no JIT; programs are compiled to a small tree with resolved
//!   variable slots (no hashing at run time).  The tree is self-contained so a bytecode or
//!   cranelift backend can be added behind [`Context::run`] later.
//!
//! # Dialects
//!
//! [`Dialect::ProjectMEval`] (the default) is the behaviour described above.
//! [`Dialect::NsEel2`] is Cockos ns-eel2 as MilkDrop 3 ships it (numeric rules, name rules, RAM
//! model, function set; see `docs/md3.md`).  The dialect is chosen when the shared
//! [`GlobalMemory`] is created ([`GlobalMemory::with_dialect`]); every [`Context`] on it follows.
//!
//! ```
//! use std::sync::Arc;
//! use lactea_eel::{Context, GlobalMemory};
//! let mut ctx = Context::new(Arc::new(GlobalMemory::new()));
//! let x = ctx.var("x");
//! let prog = ctx.compile("y = x * 2 + 1;").unwrap();
//! ctx.set(x, 20.0);
//! ctx.run(&prog);
//! let y = ctx.var("Y");
//! assert_eq!(ctx.get(y), 41.0);
//! ```

#![warn(missing_docs)]

mod context;
mod eval;
mod fast;
mod lexer;
mod memory;
mod ns;
mod parser;
mod rng;
mod tree;

pub use context::{CompileError, Context, Program, Var};
pub use memory::{GlobalMemory, MEM_ITEMS};
pub use ns::Dialect;
pub use rng::PROJECTM_DEFAULT_SEED;
