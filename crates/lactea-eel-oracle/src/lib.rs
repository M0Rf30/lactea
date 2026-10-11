//! Test-only: projectm-eval (MIT, C) built via `cc`, used as the differential oracle for
//! `lactea-eel`.  Contains a small safe FFI wrapper plus the comparison harness used by
//! the corpus / fuzz tests and the benchmark.

pub mod diff;
pub mod preset_code;

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int};
use std::rc::Rc;

#[repr(C)]
struct RawCtx {
    _p: [u8; 0],
}
#[repr(C)]
struct RawCode {
    _p: [u8; 0],
}

unsafe extern "C" {
    fn projectm_eval_memory_buffer_create() -> *mut *mut f64;
    fn projectm_eval_memory_buffer_destroy(b: *mut *mut f64);
    fn projectm_eval_context_create(gmem: *mut *mut f64, regs: *mut [f64; 100]) -> *mut RawCtx;
    fn projectm_eval_context_destroy(ctx: *mut RawCtx);
    fn projectm_eval_context_free_memory(ctx: *mut RawCtx);
    fn projectm_eval_context_reset_variables(ctx: *mut RawCtx);
    fn projectm_eval_context_register_variable(ctx: *mut RawCtx, name: *const c_char) -> *mut f64;
    fn projectm_eval_code_compile(ctx: *mut RawCtx, code: *const c_char) -> *mut RawCode;
    fn projectm_eval_code_destroy(code: *mut RawCode);
    fn projectm_eval_code_execute(code: *mut RawCode) -> f64;
    fn projectm_eval_get_error(
        ctx: *mut RawCtx,
        line: *mut c_int,
        col: *mut c_int,
    ) -> *const c_char;

    fn prjm_oracle_seed_rng(seed: u32);
    fn prjm_oracle_genrand() -> u32;
    fn prjm_oracle_var_count(ctx: *mut RawCtx) -> usize;
    fn prjm_oracle_var_at(ctx: *mut RawCtx, index: usize, value: *mut f64) -> *const c_char;
    fn prjm_oracle_megabuf_get(ctx: *mut RawCtx, index: i32) -> f64;
    fn prjm_oracle_gmegabuf_get(ctx: *mut RawCtx, index: i32) -> f64;
}

/// Seeds the (thread-local, patched) C `rand()` generator.  `0x4141f00d` is projectm-eval's
/// own initial state.
pub fn seed_rng(seed: u32) {
    unsafe { prjm_oracle_seed_rng(seed) }
}

/// Raw 32-bit output of the C Mersenne Twister.
pub fn raw_genrand() -> u32 {
    unsafe { prjm_oracle_genrand() }
}

/// gmegabuf + reg00..reg99 storage shared by oracle contexts.
pub struct OracleGlobals {
    gmem: *mut *mut f64,
    regs: *mut [f64; 100],
}

impl OracleGlobals {
    pub fn new() -> Rc<Self> {
        Rc::new(OracleGlobals {
            gmem: unsafe { projectm_eval_memory_buffer_create() },
            regs: Box::into_raw(Box::new([0.0; 100])),
        })
    }

    pub fn reg(&self, i: usize) -> f64 {
        unsafe { (*self.regs)[i] }
    }

    pub fn set_reg(&self, i: usize, v: f64) {
        unsafe { (*self.regs)[i] = v }
    }
}

impl Drop for OracleGlobals {
    fn drop(&mut self) {
        unsafe {
            projectm_eval_memory_buffer_destroy(self.gmem);
            drop(Box::from_raw(self.regs));
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleError {
    pub message: String,
    pub line: i32,
    pub column: i32,
}

/// Raw pointer to a context variable, only meaningful for the context that issued it (see
/// [`OracleContext::set_h`]); not tied to a lifetime, hence the unsafe accessors.
#[derive(Clone, Copy)]
pub struct OracleVar(*mut f64);

pub struct OracleProgram {
    raw: *mut RawCode,
}

impl Drop for OracleProgram {
    fn drop(&mut self) {
        unsafe { projectm_eval_code_destroy(self.raw) }
    }
}

pub struct OracleContext {
    raw: *mut RawCtx,
    globals: Rc<OracleGlobals>,
    vars: HashMap<String, *mut f64>,
}

impl OracleContext {
    pub fn new(globals: Rc<OracleGlobals>) -> Self {
        let raw = unsafe { projectm_eval_context_create(globals.gmem, globals.regs) };
        assert!(!raw.is_null());
        OracleContext {
            raw,
            globals,
            vars: HashMap::new(),
        }
    }

    pub fn globals(&self) -> &Rc<OracleGlobals> {
        &self.globals
    }

    fn var_ptr(&mut self, name: &str) -> *mut f64 {
        if let Some(p) = self.vars.get(name) {
            return *p;
        }
        let c = CString::new(name).expect("no NUL in variable names");
        let p = unsafe { projectm_eval_context_register_variable(self.raw, c.as_ptr()) };
        self.vars.insert(name.to_string(), p);
        p
    }

    pub fn set(&mut self, name: &str, v: f64) {
        let p = self.var_ptr(name);
        unsafe { *p = v }
    }

    /// Stable handle for fast repeated access (benchmarks).  The handle is only valid for
    /// [`set_h`](Self::set_h) / [`get_h`](Self::get_h) on this context while it is alive.
    pub fn handle(&mut self, name: &str) -> OracleVar {
        OracleVar(self.var_ptr(name))
    }

    /// Writes through a handle.
    ///
    /// # Safety
    /// `h` must come from [`handle`](Self::handle) of this very context, which must still be
    /// alive (it is, as `self`).
    #[inline]
    pub unsafe fn set_h(&mut self, h: OracleVar, v: f64) {
        // SAFETY: guaranteed by the caller; `&mut self` rules out concurrent access.
        unsafe { *h.0 = v }
    }

    /// Reads through a handle.
    ///
    /// # Safety
    /// Same contract as [`set_h`](Self::set_h).
    #[inline]
    pub unsafe fn get_h(&self, h: OracleVar) -> f64 {
        // SAFETY: guaranteed by the caller.
        unsafe { *h.0 }
    }

    pub fn get(&mut self, name: &str) -> f64 {
        let p = self.var_ptr(name);
        unsafe { *p }
    }

    pub fn compile(&mut self, code: &str) -> Result<OracleProgram, OracleError> {
        // The C API takes a NUL terminated string: truncate like the scanner would.
        let code = code.split('\0').next().unwrap_or("");
        let c = CString::new(code).unwrap();
        let raw = unsafe { projectm_eval_code_compile(self.raw, c.as_ptr()) };
        if raw.is_null() {
            let (mut line, mut col) = (0, 0);
            let msg = unsafe {
                let p = projectm_eval_get_error(self.raw, &mut line, &mut col);
                if p.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(p).to_string_lossy().into_owned()
                }
            };
            return Err(OracleError {
                message: msg,
                line,
                column: col,
            });
        }
        Ok(OracleProgram { raw })
    }

    pub fn run(&mut self, p: &OracleProgram) -> f64 {
        unsafe { projectm_eval_code_execute(p.raw) }
    }

    pub fn reset_vars(&mut self) {
        unsafe { projectm_eval_context_reset_variables(self.raw) }
    }

    pub fn free_memory(&mut self) {
        unsafe { projectm_eval_context_free_memory(self.raw) }
    }

    /// All variables the C context knows (name as first written, value).
    pub fn vars(&self) -> Vec<(String, f64)> {
        let n = unsafe { prjm_oracle_var_count(self.raw) };
        (0..n)
            .map(|i| {
                let mut v = 0.0;
                let p = unsafe { prjm_oracle_var_at(self.raw, i, &mut v) };
                assert!(!p.is_null(), "var_at({i}) < var_count returned NULL");
                (
                    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned(),
                    v,
                )
            })
            .collect()
    }

    pub fn megabuf(&self, i: i32) -> f64 {
        unsafe { prjm_oracle_megabuf_get(self.raw, i) }
    }

    pub fn gmegabuf(&self, i: i32) -> f64 {
        unsafe { prjm_oracle_gmegabuf_get(self.raw, i) }
    }
}

impl Drop for OracleContext {
    fn drop(&mut self) {
        unsafe { projectm_eval_context_destroy(self.raw) }
    }
}
