//! Differential harness: run the same code in `lactea-eel` and in projectm-eval (C) with
//! identical inputs and compare every observable bitwise (NaN == NaN).

use crate::{OracleContext, OracleGlobals, OracleProgram};
use lactea_eel::{Context, GlobalMemory, PROJECTM_DEFAULT_SEED, Program};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

/// Bitwise equality, any NaN equals any NaN.
pub fn same(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

/// xorshift64* - tiny deterministic generator for test inputs.
pub struct Rng(pub u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// [0, 1)
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

/// FNV-1a, for stable per-preset seeds.
pub fn hash_str(s: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Variable names typically provided by the MilkDrop engine to the different code blocks.
pub const INPUT_VARS: &[&str] = &[
    "time",
    "frame",
    "fps",
    "bass",
    "mid",
    "treb",
    "bass_att",
    "mid_att",
    "treb_att",
    "progress",
    "decay",
    "gamma",
    "echo_zoom",
    "echo_alpha",
    "echo_orient",
    "wave_mode",
    "wave_a",
    "wave_r",
    "wave_g",
    "wave_b",
    "wave_x",
    "wave_y",
    "wave_mystery",
    "wave_usedots",
    "wave_thick",
    "wave_additive",
    "wave_brighten",
    "zoom",
    "zoomexp",
    "rot",
    "cx",
    "cy",
    "dx",
    "dy",
    "warp",
    "sx",
    "sy",
    "ob_size",
    "ob_r",
    "ob_g",
    "ob_b",
    "ob_a",
    "ib_size",
    "ib_r",
    "ib_g",
    "ib_b",
    "ib_a",
    "mv_x",
    "mv_y",
    "mv_dx",
    "mv_dy",
    "mv_l",
    "mv_r",
    "mv_g",
    "mv_b",
    "mv_a",
    "monitor",
    "meshx",
    "meshy",
    "pixelsx",
    "pixelsy",
    "aspectx",
    "aspecty",
    "blur1_min",
    "blur1_max",
    "blur2_min",
    "blur2_max",
    "blur3_min",
    "blur3_max",
    "blur1_edge_darken",
    "x",
    "y",
    "rad",
    "ang",
    "sample",
    "value1",
    "value2",
    "r",
    "g",
    "b",
    "a",
    "r2",
    "g2",
    "b2",
    "a2",
    "border_r",
    "border_g",
    "border_b",
    "border_a",
    "sides",
    "thick",
    "additive",
    "textured",
    "tex_zoom",
    "tex_ang",
    "instance",
    "num_inst",
    "t1",
    "t2",
    "t3",
    "t4",
    "t5",
    "t6",
    "t7",
    "t8",
    "q1",
    "q2",
    "q3",
    "q4",
    "q5",
    "q6",
    "q7",
    "q8",
    "q9",
    "q10",
    "q11",
    "q12",
    "q13",
    "q14",
    "q15",
    "q16",
    "q17",
    "q18",
    "q19",
    "q20",
    "q21",
    "q22",
    "q23",
    "q24",
    "q25",
    "q26",
    "q27",
    "q28",
    "q29",
    "q30",
    "q31",
    "q32",
    "bass_smooth",
    "treb_smooth",
    "mid_smooth",
    "beat",
    "red",
    "green",
    "blue",
    "alpha",
    "size",
    "scale",
    "rotation",
];

const SPECIAL: &[f64] = &[
    0.0, 1.0, -1.0, 0.5, 2.0, -0.5, 1e-6, -1e-6, 1e-5, 1e3, -1e3, 1e9, 3.0, 7.0, 255.0, 0.999999,
    4.00005,
];

/// Plausible value for a MilkDrop input variable.
pub fn input_value(name: &str, rng: &mut Rng, iter: usize) -> f64 {
    match name {
        "time" => iter as f64 / 60.0,
        "frame" => iter as f64,
        "fps" => 60.0,
        "progress" => rng.unit(),
        "meshx" => 48.0,
        "meshy" => 36.0,
        "pixelsx" => 640.0,
        "pixelsy" => 360.0,
        "aspectx" | "aspecty" => 1.0,
        "x" | "y" => rng.unit(),
        "rad" => rng.range(0.0, 1.5),
        "ang" => rng.range(0.0, std::f64::consts::TAU),
        "sample" => rng.unit(),
        "instance" => rng.below(8) as f64,
        "num_inst" => 8.0,
        "bass" | "mid" | "treb" | "bass_att" | "mid_att" | "treb_att" => rng.range(0.0, 2.5),
        _ => {
            if rng.below(10) == 0 {
                SPECIAL[rng.below(SPECIAL.len() as u64) as usize]
            } else {
                rng.range(-1.0, 1.0) * if rng.below(4) == 0 { 10.0 } else { 1.0 }
            }
        }
    }
}

/// One engine on each side, with identical RNG state and shared-memory layout.
pub struct Pair {
    pub rust: Context,
    pub rglobals: Arc<GlobalMemory>,
    pub c: OracleContext,
    pub cglobals: Rc<OracleGlobals>,
}

pub struct Compiled {
    pub rust: Program,
    pub c: OracleProgram,
}

pub enum CompileOutcome {
    Both(Compiled),
    BothFailed { rust: String, c: String },
    RustOnly(String),
    COnly(String),
}

impl Pair {
    pub fn new() -> Self {
        crate::seed_rng(PROJECTM_DEFAULT_SEED);
        let rglobals = Arc::new(GlobalMemory::new());
        let cglobals = OracleGlobals::new();
        Pair {
            rust: Context::new(rglobals.clone()),
            rglobals,
            c: OracleContext::new(cglobals.clone()),
            cglobals,
        }
    }

    /// Second context sharing the globals of `other` (like several contexts of one engine).
    pub fn sibling(other: &Pair) -> Pair {
        Pair {
            rust: Context::new(other.rglobals.clone()),
            rglobals: other.rglobals.clone(),
            c: OracleContext::new(other.cglobals.clone()),
            cglobals: other.cglobals.clone(),
        }
    }

    pub fn reseed(&mut self, seed: u32) {
        crate::seed_rng(seed);
        self.rust.set_rng_seed(u64::from(seed));
    }

    pub fn compile(&mut self, code: &str) -> CompileOutcome {
        let r = self.rust.compile(code);
        let c = self.c.compile(code);
        match (r, c) {
            (Ok(rust), Ok(c)) => CompileOutcome::Both(Compiled { rust, c }),
            (Err(re), Err(ce)) => CompileOutcome::BothFailed {
                rust: re.to_string(),
                c: ce.message,
            },
            (Ok(_), Err(ce)) => CompileOutcome::COnly(ce.message),
            (Err(re), Ok(_)) => CompileOutcome::RustOnly(re.to_string()),
        }
    }

    pub fn set(&mut self, name: &str, v: f64) {
        let var = self.rust.var(name);
        self.rust.set(var, v);
        self.c.set(name, v);
    }

    /// Sets the MilkDrop input variables that exist in the context.
    pub fn set_inputs(&mut self, rng: &mut Rng, iter: usize) {
        let names: Vec<String> = self
            .rust
            .vars()
            .map(|(n, _)| n.to_string())
            .filter(|n| INPUT_VARS.contains(&n.as_str()))
            .collect();
        for n in names {
            let v = input_value(&n, rng, iter);
            self.set(&n, v);
        }
    }

    pub fn run(&mut self, p: &Compiled) -> (f64, f64) {
        (self.rust.run(&p.rust), self.c.run(&p.c))
    }

    /// Compares return values and all observable state.
    pub fn compare(&mut self, rv: f64, cv: f64) -> Result<(), Mismatch> {
        if !same(rv, cv) {
            return Err(Mismatch::new("return", "return value".into(), rv, cv));
        }
        let rvars: BTreeMap<String, f64> =
            self.rust.vars().map(|(n, v)| (n.to_string(), v)).collect();
        let cvars: BTreeMap<String, f64> = self
            .c
            .vars()
            .into_iter()
            .map(|(n, v)| (n.to_ascii_lowercase(), v))
            .collect();
        for (n, rv) in &rvars {
            match cvars.get(n) {
                None => {
                    return Err(Mismatch::new(
                        "varset",
                        format!("variable {n} missing in C"),
                        *rv,
                        f64::NAN,
                    ));
                }
                Some(cv) if !same(*rv, *cv) => {
                    return Err(Mismatch::new("var", format!("variable {n}"), *rv, *cv));
                }
                _ => {}
            }
        }
        for (n, cv) in &cvars {
            if !rvars.contains_key(n) {
                return Err(Mismatch::new(
                    "varset",
                    format!("variable {n} missing in Rust"),
                    f64::NAN,
                    *cv,
                ));
            }
        }
        for i in 0..100 {
            let (a, b) = (self.rglobals.reg(i), self.cglobals.reg(i));
            if !same(a, b) {
                return Err(Mismatch::new("reg", format!("reg{i:02}"), a, b));
            }
        }
        for i in 0..1024 {
            let (a, b) = (self.rust.megabuf(i), self.c.megabuf(i as i32));
            if !same(a, b) {
                return Err(Mismatch::new("megabuf", format!("megabuf[{i}]"), a, b));
            }
            let (a, b) = (self.rglobals.gmem(i), self.c.gmegabuf(i as i32));
            if !same(a, b) {
                return Err(Mismatch::new("gmegabuf", format!("gmegabuf[{i}]"), a, b));
            }
        }
        Ok(())
    }
}

impl Default for Pair {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct Mismatch {
    /// Coarse category (`return`, `var`, `varset`, `reg`, `megabuf`, `gmegabuf`).
    pub kind: &'static str,
    pub detail: String,
    pub rust: f64,
    pub c: f64,
}

impl Mismatch {
    fn new(kind: &'static str, detail: String, rust: f64, c: f64) -> Self {
        Mismatch {
            kind,
            detail,
            rust,
            c,
        }
    }

    /// Numeric relation of the two values.
    pub fn relation(&self) -> &'static str {
        let (a, b) = (self.rust, self.c);
        if a.is_nan() != b.is_nan() {
            "nan-vs-number"
        } else if a.is_infinite() || b.is_infinite() {
            "inf-vs-finite"
        } else if a == b {
            "signed-zero"
        } else if (a.to_bits() as i64)
            .wrapping_sub(b.to_bits() as i64)
            .unsigned_abs()
            <= 4
            && a.signum() == b.signum()
        {
            "few-ulp"
        } else {
            "different"
        }
    }
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: rust={:e} ({:#018x}) c={:e} ({:#018x}) [{}]",
            self.detail,
            self.rust,
            self.rust.to_bits(),
            self.c,
            self.c.to_bits(),
            self.relation()
        )
    }
}
