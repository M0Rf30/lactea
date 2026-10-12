//! Public API: [`Context`], [`Var`], [`Program`], [`CompileError`].

use crate::fast::FNode;
use crate::memory::{GlobalMemory, LocalMem};
use crate::ns::Dialect;
use crate::tree::Loc;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Source of unique [`Context`] ids; 64 bits so an id can never wrap onto a live context.
static NEXT_CTX_ID: AtomicU64 = AtomicU64::new(1);

/// Handle to a variable of one [`Context`]: a direct slot, resolved once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Var(u32);

const REG_FLAG: u32 = 0x8000_0000;

/// Significant length of an ns-eel2 variable name.
const NS_NAME_LEN: usize = 128;

/// `Some(NN)` if `name` is `regNN` (two decimal digits, ASCII case-insensitive).
fn parse_reg(name: &str) -> Option<u32> {
    let b = name.as_bytes();
    if b.len() == 5
        && b[..3].eq_ignore_ascii_case(b"reg")
        && b[3].is_ascii_digit()
        && b[4].is_ascii_digit()
    {
        Some(u32::from(b[3] - b'0') * 10 + u32::from(b[4] - b'0'))
    } else {
        None
    }
}

impl Var {
    /// `Some(n)` if this is the global register `regNN` (`n < 100`), else `None`.
    pub fn reg_index(self) -> Option<usize> {
        (self.0 & REG_FLAG != 0).then_some((self.0 & !REG_FLAG) as usize)
    }
}

/// Compile failure (syntax error, unknown arity, ...).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileError {
    /// Human readable description of the failure.
    pub message: String,
    /// 1-based line of the offending token.
    pub line: u32,
    /// 0-based byte column of the offending token within its line.
    pub column: u32,
    /// 0-based byte column one past the end of the offending token.
    pub column_end: u32,
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.column + 1, self.message)
    }
}

impl std::error::Error for CompileError {}

/// A compiled code block.  Only runnable by the [`Context`] that compiled it: [`Context::run`]
/// checks the owning context's id and panics on a mismatch.
pub struct Program {
    pub(crate) root: Option<FNode>,
    pub(crate) nscratch: u32,
    pub(crate) ctx_id: u64,
}

impl Program {
    pub(crate) fn new(root: Option<FNode>, nscratch: u32, ctx_id: u64) -> Self {
        Program {
            root,
            nscratch,
            ctx_id,
        }
    }

    /// True for programs without any code (empty / comments only).
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }
}

impl fmt::Debug for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Program")
            .field("empty", &self.root.is_none())
            .field("scratch", &self.nscratch)
            .finish()
    }
}

/// One execution context (variables + megabuf); the engine creates one per code kind
/// (per-frame, per-vertex, each custom wave / shape ...), all sharing one [`GlobalMemory`].
pub struct Context {
    id: u64,
    dialect: Dialect,
    pub(crate) global: Arc<GlobalMemory>,
    lookup: HashMap<String, u32>,
    names: Vec<String>,
    pub(crate) vars: Vec<f64>,
    pub(crate) scratch: Vec<f64>,
    pub(crate) mem: LocalMem,
    pub(crate) max_depth: u32,
}

impl Context {
    /// New context with no variables, sharing `global` with the other contexts of the engine.
    pub fn new(global: Arc<GlobalMemory>) -> Self {
        Context {
            id: NEXT_CTX_ID.fetch_add(1, Ordering::Relaxed),
            dialect: global.dialect(),
            global,
            lookup: HashMap::new(),
            names: Vec::new(),
            vars: Vec::new(),
            scratch: vec![0.0; 16],
            mem: LocalMem::new(),
            max_depth: crate::parser::DEFAULT_MAX_DEPTH,
        }
    }

    /// Maximum expression tree depth accepted by [`compile`](Self::compile) (default 1000).
    /// Deeper programs are rejected with a [`CompileError`].
    ///
    /// The parser and interpreter are recursive, so the limit is only as safe as the stack of
    /// the calling thread: raising it far above the default can overflow the stack (abort)
    /// before the limit is reached.  Size the thread stack accordingly when raising it.
    pub fn set_max_depth(&mut self, depth: u32) {
        self.max_depth = depth;
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// The memory shared by this context and its siblings.
    pub fn global(&self) -> &Arc<GlobalMemory> {
        &self.global
    }

    /// The dialect this context compiles and runs (that of its [`GlobalMemory`]).
    pub fn dialect(&self) -> Dialect {
        self.dialect
    }

    /// Resolves (creating if needed) a variable by case-insensitive name.  `reg00..reg99`
    /// resolve to the shared global registers.
    pub fn var(&mut self, name: &str) -> Var {
        if let Some(idx) = parse_reg(name) {
            return Var(REG_FLAG | idx);
        }
        let key = self.key(name);
        if let Some(&i) = self.lookup.get(&key) {
            return Var(i);
        }
        let i = self.vars.len() as u32;
        self.vars.push(0.0);
        self.names.push(key.clone());
        self.lookup.insert(key, i);
        Var(i)
    }

    /// Like [`var`](Self::var) but does not create.
    pub fn find_var(&self, name: &str) -> Option<Var> {
        if let Some(idx) = parse_reg(name) {
            return Some(Var(REG_FLAG | idx));
        }
        self.lookup.get(&self.key(name)).map(|&i| Var(i))
    }

    /// Table key of a variable name: lower-case; ns-eel2 only distinguishes the first 128 bytes.
    fn key(&self, name: &str) -> String {
        let mut key = name.to_ascii_lowercase();
        if self.dialect == Dialect::NsEel2 && key.len() > NS_NAME_LEN {
            let mut cut = NS_NAME_LEN;
            while !key.is_char_boundary(cut) {
                cut -= 1;
            }
            key.truncate(cut);
        }
        key
    }

    pub(crate) fn loc_of(&self, v: Var) -> Loc {
        match v.reg_index() {
            Some(i) => Loc::Reg(i as u8),
            None => Loc::Var(v.0),
        }
    }

    /// Current value of `v`: a context-local variable or a shared register.  `v` must come
    /// from this context (a `Var` of another context may index out of range and panic).
    #[inline]
    pub fn get(&self, v: Var) -> f64 {
        if v.0 & REG_FLAG == 0 {
            self.vars[v.0 as usize]
        } else {
            self.global.reg((v.0 & !REG_FLAG) as usize)
        }
    }

    /// Sets `v` to `value`; same contract as [`get`](Self::get).
    #[inline]
    pub fn set(&mut self, v: Var, value: f64) {
        if v.0 & REG_FLAG == 0 {
            self.vars[v.0 as usize] = value;
        } else {
            self.global.set_reg((v.0 & !REG_FLAG) as usize, value);
        }
    }

    /// All context-local variables (lower-cased names) with their current values, in creation
    /// order.  `regNN` globals live in [`GlobalMemory`].
    pub fn vars(&self) -> impl Iterator<Item = (&str, f64)> + '_ {
        self.names
            .iter()
            .map(String::as_str)
            .zip(self.vars.iter().copied())
    }

    /// Number of context-local variables.
    pub fn var_count(&self) -> usize {
        self.vars.len()
    }

    /// Name of the context-local variable behind `v` (None for regs).
    pub fn var_name(&self, v: Var) -> Option<&str> {
        if v.reg_index().is_some() {
            None
        } else {
            self.names.get(v.0 as usize).map(String::as_str)
        }
    }

    /// Sets all context-local variables to 0 (`projectm_eval_context_reset_variables`).
    /// Registers and memory are untouched.
    pub fn reset_vars(&mut self) {
        self.vars.fill(0.0);
    }

    /// Frees the local megabuf (`projectm_eval_context_free_memory`).
    pub fn free_memory(&mut self) {
        self.mem.clear();
    }

    /// Reads megabuf slot `index` without side effects (unallocated reads 0).
    pub fn megabuf(&self, index: usize) -> f64 {
        self.mem.peek(index)
    }

    /// Reseeds the shared `rand()` stream.  The generator is MT19937 exactly like
    /// projectm-eval; seeds up to `u32::MAX` are used verbatim, larger seeds are folded with
    /// `lo ^ hi`.  `Context::new` starts from projectm-eval's own initial state
    /// (`0x4141f00d`), so `set_rng_seed(0x4141f00d)` is a no-op reset to that state.
    ///
    /// Note the stream is shared by every context using the same [`GlobalMemory`], like the
    /// process-global static in projectm-eval.
    pub fn set_rng_seed(&mut self, seed: u64) {
        self.global.set_rng_seed(seed);
    }

    pub(crate) fn ensure_scratch(&mut self, n: usize) {
        if self.scratch.len() < n {
            self.scratch.resize(n, 0.0);
        }
    }

    /// Compiles `src`.  Variables referenced by the code are created in this context
    /// (also when compilation fails part-way, like projectm-eval).
    pub fn compile(&mut self, src: &str) -> Result<Program, CompileError> {
        let p = crate::parser::compile(self, src)?;
        // Scratch cells only ever grow, so `run` never has to check.
        self.ensure_scratch(p.nscratch as usize);
        Ok(p)
    }

    /// Runs a program compiled by this context and returns the value of its last expression.
    ///
    /// # Panics
    /// If `prog` was compiled by another context.
    pub fn run(&mut self, prog: &Program) -> f64 {
        assert_eq!(
            prog.ctx_id, self.id,
            "Program belongs to a different Context"
        );
        let Some(root) = &prog.root else {
            return 0.0;
        };
        self.fval(root)
    }
}
