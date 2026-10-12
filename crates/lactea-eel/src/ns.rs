//! The ns-eel2 numeric rules (MilkDrop 3 3.37, docs/md3.md "Expression engine").
//!
//! Everything here is a pure function of its operands; the node variants that call them are chosen
//! at compile time ([`Dialect::NsEel2`]), so the projectm-eval paths in `eval.rs` stay untouched.
//!
//! x87 `fistp` conversions: the MD3 binary never changes the FPU control word, so the conversions
//! of `%`, `loop`, `&`, `|` and the RAM index use the default rounding, round-to-nearest-even
//! (spec E, Annex A §3.1 / OQ1: inferred, not measured).  [`fistp32`] / [`fistp64`] implement
//! that, including the "integer indefinite" result (`i32::MIN` / `i64::MIN`) for NaN and values
//! that do not fit.  The SSE `cvttsd2si` truncation of `memcpy`/`memset` is `eval::cvt_i32`.

use crate::eval::CLOSE;

/// Which expression language flavour a [`Context`](crate::Context) compiles and runs.
///
/// The dialect rides in the [`GlobalMemory`](crate::GlobalMemory) the contexts share (contexts of
/// one preset must agree on it: the RAM model is part of the dialect).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Dialect {
    /// projectm-eval 1.0.8, what projectM runs (and what `lactea-eel-oracle` compares against).
    #[default]
    ProjectMEval,
    /// Cockos ns-eel2 as MilkDrop 3 ships it: unguarded `/`, assignment filtering of
    /// NaN / Inf / denormals, integer `%`, `|x| >= 1e-5` truthiness, `$PI` constants only,
    /// 8M-entry `megabuf`, wrapping 1M-entry `gmegabuf`, 53 functions; `.` is not an identifier character.
    NsEel2,
}

/// `fistp dword` under round-to-nearest-even; NaN / out of range give the "integer indefinite".
#[inline]
pub(crate) fn fistp32(x: f64) -> i32 {
    let r = x.round_ties_even();
    if (-2147483648.0..2147483648.0).contains(&r) {
        r as i32
    } else {
        i32::MIN
    }
}

/// `fistp qword` under round-to-nearest-even; NaN / out of range give `i64::MIN`.
#[inline]
pub(crate) fn fistp64(x: f64) -> i64 {
    let r = x.round_ties_even();
    if (-9223372036854775808.0..9223372036854775808.0).contains(&r) {
        r as i64
    } else {
        i64::MIN
    }
}

/// Truthiness of `if`, `&&`, `||`, `while`, `!`: false iff `|x| < 1e-5`, NaN counts as false.
#[inline(always)]
pub(crate) fn truthy(x: f64) -> bool {
    x.abs() >= CLOSE
}

/// `_set`: stores the value unchanged unless its biased exponent is 0 (±0, denormal) or all ones
/// (±Inf, NaN), which store `+0.0`.
#[inline(always)]
pub(crate) fn assign_filter(v: f64) -> f64 {
    let e = (v.to_bits() >> 52) & 0x7ff;
    if e == 0 || e == 0x7ff { 0.0 } else { v }
}

/// `%`: `|a|` and `|b|` rounded to 32-bit, unsigned remainder, zero divisor gives 0.
#[inline]
pub(crate) fn modulo(a: f64, b: f64) -> f64 {
    let ia = fistp32(a.abs()) as u32;
    let ib = fistp32(b.abs()) as u32;
    if ib == 0 { 0.0 } else { f64::from(ia % ib) }
}

/// `invsqrt`: the Quake step with a float32 magic number, the Newton step in double.
#[inline]
pub(crate) fn invsqrt(a: f64) -> f64 {
    let bits = (a as f32).to_bits() as i32 >> 1;
    let y = f64::from(f32::from_bits(0x5f37_59df_i32.wrapping_sub(bits) as u32));
    ((a * -0.5) * y * y + 1.5) * y
}

/// `sign`: ±0 returns its input (so `-0` survives until it is stored), anything else (NaN
/// included) is +1 / -1 by its sign bit.
#[inline]
pub(crate) fn sign(a: f64) -> f64 {
    let bits = a.to_bits();
    if bits << 1 == 0 {
        a
    } else if bits >> 63 != 0 {
        -1.0
    } else {
        1.0
    }
}

/// `_equal` / `_noteq`: the comparison `|a-b| < 1e-5` as the x87 reports it, where an unordered
/// result (NaN) counts as "less"; `!=` is its complement.
#[inline]
pub(crate) fn close(a: f64, b: f64) -> bool {
    let d = (a - b).abs();
    d.is_nan() || d < CLOSE
}

/// Index of a `megabuf` access: `fistp(i + 1e-5)`, valid for `0 <= w < 8 * 1024 * 1024`.
#[inline]
pub(crate) fn mem_index(x: f64) -> Option<u32> {
    let w = fistp32(x + CLOSE);
    if w >= 0 && ((w as usize) >> 16) < crate::memory::BLOCKS {
        Some(w as u32)
    } else {
        None
    }
}

/// Index of a `gmegabuf` access: the same conversion, masked to the 1,048,576-entry array.
#[inline]
pub(crate) fn gmem_index(x: f64) -> u32 {
    (fistp32(x + CLOSE) as u32) & 0xf_ffff
}
