//! Tree-walking interpreter reproducing TreeFunctions.c semantics (including its reference-
//! passing quirks) in safe Rust.

use crate::context::Context;
use crate::memory::{BLOCK_MASK, BLOCKS};
use crate::tree::{AssignOp, BinOp, Loc, Node, UnOp};
use std::sync::atomic::Ordering::Relaxed;

/// `COMPARE_CLOSEFACTOR`
pub(crate) const CLOSE: f64 = 0.00001;
/// `MAX_LOOP_COUNT`
pub(crate) const MAX_LOOP: i64 = 1_048_576;

/// C `(int32_t)double` as compiled for x86-64 (`cvttsd2si`): out of range / NaN -> `i32::MIN`.
#[inline]
pub(crate) fn cvt_i32(x: f64) -> i32 {
    if x > -2147483649.0 && x < 2147483648.0 {
        x as i32
    } else {
        i32::MIN
    }
}

/// C `(int64_t)double` as compiled for x86-64: out of range / NaN -> `i64::MIN`.
#[inline]
pub(crate) fn cvt_i64(x: f64) -> i64 {
    if (-9223372036854775808.0..9223372036854775808.0).contains(&x) {
        x as i64
    } else {
        i64::MIN
    }
}

/// Megabuf index for a C `(int32_t)(x + 0.0001)` access, `None` if it maps to no block.
#[inline]
pub(crate) fn mem_index(x: f64) -> Option<u32> {
    let i = cvt_i32(x + 0.0001);
    if i >= 0 && ((i as usize) >> 16) < BLOCKS {
        Some(i as u32)
    } else {
        None
    }
}

#[inline(always)]
pub(crate) fn b2f(b: bool) -> f64 {
    if b { 1.0 } else { 0.0 }
}

#[inline]
fn pow_checked(x: f64, y: f64) -> f64 {
    if x.abs() < CLOSE && y < 0.0 {
        return 0.0;
    }
    let r = x.powf(y);
    if r.is_nan() { 0.0 } else { r }
}

#[inline]
pub(crate) fn bin(op: BinOp, x: f64, y: f64) -> f64 {
    match op {
        BinOp::Add => x + y,
        BinOp::Sub => x - y,
        BinOp::Mul => x * y,
        BinOp::Div => {
            if y.abs() < CLOSE {
                0.0
            } else {
                x / y
            }
        }
        BinOp::Mod => {
            let d = cvt_i64(y);
            if d == 0 {
                0.0
            } else {
                // C would trap on INT64_MIN % -1; wrapping_rem yields 0 instead.
                cvt_i64(x).wrapping_rem(d) as f64
            }
        }
        BinOp::Pow => pow_checked(x, y),
        BinOp::Min => {
            if x < y {
                x
            } else {
                y
            }
        }
        BinOp::Max => {
            if x > y {
                x
            } else {
                y
            }
        }
        BinOp::Atan2 => x.atan2(y),
        BinOp::Sigmoid => {
            let t = 1.0 + (-x * y).exp();
            if t.abs() > CLOSE { 1.0 / t } else { 0.0 }
        }
        BinOp::Equal => b2f((x - y).abs() < CLOSE),
        BinOp::NotEq => b2f((x - y).abs() > CLOSE),
        BinOp::Below => b2f(x < y),
        BinOp::Above => b2f(x > y),
        BinOp::BelEq => b2f(x <= y),
        BinOp::AboEq => b2f(x >= y),
        BinOp::Band => b2f(x.abs() > CLOSE && y.abs() > CLOSE),
        BinOp::Bor => b2f(x.abs() > CLOSE || y.abs() > CLOSE),
        BinOp::BitOr => (cvt_i64(x) | cvt_i64(y)) as f64,
        BinOp::BitAnd => (cvt_i64(x) & cvt_i64(y)) as f64,
    }
}

#[inline]
pub(crate) fn assign_op(op: AssignOp, cur: f64, r: f64) -> f64 {
    match op {
        AssignOp::Add => cur + r,
        AssignOp::Sub => cur - r,
        AssignOp::Mul => cur * r,
        AssignOp::Div => {
            if r.abs() < CLOSE {
                0.0
            } else {
                cur / r
            }
        }
        AssignOp::Mod => {
            let d = cvt_i64(r);
            if d == 0 {
                0.0
            } else {
                cvt_i64(cur).wrapping_rem(d) as f64
            }
        }
        AssignOp::Pow => pow_checked(cur, r),
        AssignOp::Or => (cvt_i64(cur) | cvt_i64(r)) as f64,
        AssignOp::And => (cvt_i64(cur) & cvt_i64(r)) as f64,
    }
}

impl Context {
    #[inline]
    pub(crate) fn un(&mut self, op: UnOp, x: f64) -> f64 {
        match op {
            UnOp::Sin => x.sin(),
            UnOp::Cos => x.cos(),
            UnOp::Tan => x.tan(),
            UnOp::Asin => {
                if x < -1.0 || x > 1.0 {
                    0.0
                } else {
                    x.asin()
                }
            }
            UnOp::Acos => {
                if x < -1.0 || x > 1.0 {
                    0.0
                } else {
                    x.acos()
                }
            }
            UnOp::Atan => x.atan(),
            UnOp::Sqr => x * x,
            UnOp::Sqrt => x.abs().sqrt(),
            UnOp::Exp => x.exp(),
            UnOp::Neg => -x,
            UnOp::Log => {
                if x <= 0.0 {
                    0.0
                } else {
                    x.ln()
                }
            }
            UnOp::Log10 => {
                if x <= 0.0 {
                    0.0
                } else {
                    x.log10()
                }
            }
            UnOp::Abs => x.abs(),
            UnOp::Sign => {
                if x == 0.0 {
                    0.0
                } else if x < 0.0 {
                    -1.0
                } else {
                    1.0
                }
            }
            UnOp::Rand => {
                let mut m = x.floor();
                if m < 1.0 {
                    m = 1.0;
                }
                f64::from(self.global.rand_u32()) * (1.0 / 4294967295.0) * m
            }
            UnOp::Floor => x.floor(),
            UnOp::Ceil => x.ceil(),
            UnOp::Invsqrt => {
                let num2 = x * 0.5;
                let bits = 0x5fe6_eb50_c7b5_37a9u64.wrapping_sub(x.to_bits() >> 1);
                let mut y = f64::from_bits(bits);
                y *= 1.5 - (num2 * y * y);
                if y.is_nan() { 0.0 } else { y }
            }
            UnOp::Bnot => b2f(x.abs() < CLOSE),
        }
    }

    #[inline]
    pub(crate) fn read(&self, l: Loc) -> f64 {
        match l {
            Loc::Scratch(i) => self.scratch[i as usize],
            Loc::Var(i) => self.vars[i as usize],
            Loc::Reg(i) => self.global.reg(i as usize),
            Loc::Mem(i) => self.mem.get(i),
            Loc::GMem(i) => self.global.gmem_get(i),
        }
    }

    #[inline]
    pub(crate) fn write(&mut self, l: Loc, v: f64) {
        match l {
            Loc::Scratch(i) => self.scratch[i as usize] = v,
            Loc::Var(i) => self.vars[i as usize] = v,
            Loc::Reg(i) => self.global.set_reg(i as usize, v),
            Loc::Mem(i) => self.mem.set(i, v),
            Loc::GMem(i) => {
                let i = i as usize;
                self.global.block(i >> 16)[i & BLOCK_MASK].store(v.to_bits(), Relaxed);
            }
        }
    }

    /// Evaluate both operands of a binary function.  The C code evaluates arg0, then arg1, and
    /// only afterwards dereferences both result pointers.
    #[inline]
    fn pair(&mut self, a: &Node, b: &Node, defer: bool) -> (f64, f64) {
        if defer {
            let la = self.place(a, Loc::Scratch(a.out_slot()));
            let vb = self.val(b);
            (self.read(la), vb)
        } else {
            let x = self.val(a);
            let y = self.val(b);
            (x, y)
        }
    }

    /// Evaluate an instruction whose result is discarded.
    #[inline]
    fn discard(&mut self, n: &Node, s: u32) {
        if n.is_place() {
            let _ = self.place(n, Loc::Scratch(s));
        } else {
            let _ = self.val(n);
        }
    }

    /// Value of a node evaluated with a private out cell.
    pub(crate) fn val(&mut self, n: &Node) -> f64 {
        match n {
            Node::Const(v) => *v,
            Node::Var(l) => self.read(*l),
            Node::Un(op, a) => {
                let x = self.val(a);
                self.un(*op, x)
            }
            Node::Bin { op, args, defer } => {
                let (x, y) = self.pair(&args[0], &args[1], *defer);
                bin(*op, x, y)
            }
            Node::And(args) => {
                if self.val(&args[0]).abs() > CLOSE {
                    b2f(self.val(&args[1]).abs() > CLOSE)
                } else {
                    0.0
                }
            }
            Node::Or(args) => {
                if self.val(&args[0]).abs() < CLOSE {
                    b2f(self.val(&args[1]).abs() > CLOSE)
                } else {
                    1.0
                }
            }
            Node::If { args, .. } => {
                if self.val(&args[0]) != 0.0 {
                    self.val(&args[1])
                } else {
                    self.val(&args[2])
                }
            }
            Node::Exec2 { s, args } => {
                self.discard(&args[0], *s);
                self.val(&args[1])
            }
            Node::List { s, items } => {
                let (last, init) = items.split_last().expect("non-empty list");
                for it in init {
                    self.discard(it, *s);
                }
                self.val(last)
            }
            Node::FreeMbuf { arg } => self.val(arg),
            _ => {
                let l = self.place(n, Loc::Scratch(n.out_slot()));
                self.read(l)
            }
        }
    }

    /// Evaluate `n`; `out` is the cell the caller offered for plain results.  Returns where the
    /// result actually lives (a variable / memory cell for references, `out` otherwise).
    pub(crate) fn place(&mut self, n: &Node, out: Loc) -> Loc {
        match n {
            Node::Var(l) => *l,
            Node::If { args, .. } => {
                if self.val(&args[0]) != 0.0 {
                    self.place(&args[1], out)
                } else {
                    self.place(&args[2], out)
                }
            }
            Node::Exec2 { s, args } => {
                self.discard(&args[0], *s);
                self.place(&args[1], out)
            }
            Node::Exec3 { s, args } => {
                // The C code passes the same out-pointer to arg0 and arg1.
                let mut vp = Loc::Scratch(*s);
                vp = self.place(&args[0], vp);
                vp = self.place(&args[1], vp);
                let _ = vp;
                self.place(&args[2], out)
            }
            Node::List { s, items } => {
                let (last, init) = items.split_last().expect("non-empty list");
                for it in init {
                    self.discard(it, *s);
                }
                self.place(last, Loc::Scratch(*s))
            }
            Node::Loop { s, args } => {
                let mut vp = Loc::Scratch(*s);
                vp = self.place(&args[0], vp);
                let mut n = cvt_i64(self.read(vp));
                if n > MAX_LOOP {
                    n = MAX_LOOP;
                }
                let mut i = 0;
                while i < n {
                    vp = self.place(&args[1], Loc::Scratch(*s));
                    i += 1;
                }
                vp
            }
            Node::While { s, arg } => {
                let mut vp = Loc::Scratch(*s);
                let mut left = MAX_LOOP;
                loop {
                    vp = self.place(arg, vp);
                    if self.read(vp).abs() > CLOSE {
                        left -= 1;
                        if left != 0 {
                            continue;
                        }
                    }
                    break;
                }
                vp
            }
            Node::Set { args, .. } => {
                let lp = self.place(&args[0], out);
                let v = self.val(&args[1]);
                self.write(lp, v);
                lp
            }
            Node::OpAssign { op, args, .. } => {
                let lp = self.place(&args[0], out);
                let r = self.val(&args[1]);
                let cur = self.read(lp);
                let v = assign_op(*op, cur, r);
                self.write(lp, v);
                lp
            }
            Node::Mem { global, arg, .. } => {
                let i = cvt_i32(self.val(arg) + 0.0001);
                if i >= 0 && ((i as usize) >> 16) < BLOCKS {
                    if *global {
                        let _ = self.global.block((i as usize) >> 16);
                        Loc::GMem(i as u32)
                    } else {
                        self.mem.ensure((i as usize) >> 16);
                        Loc::Mem(i as u32)
                    }
                } else {
                    self.write(out, 0.0);
                    out
                }
            }
            Node::FreeMbuf { arg } => self.place(arg, out),
            Node::Memcpy { s, args } => {
                let dp = self.place(&args[0], Loc::Scratch(*s));
                let sp = self.place(&args[1], Loc::Scratch(*s + 1));
                let cp = self.place(&args[2], Loc::Scratch(*s + 2));
                let dest = cvt_i32(self.read(dp) + 0.0001);
                let src = cvt_i32(self.read(sp) + 0.0001);
                let count = cvt_i32(self.read(cp) + 0.0001);
                self.mem.copy(dest, src, count);
                dp
            }
            Node::Memset { s, args } => {
                let dp = self.place(&args[0], Loc::Scratch(*s));
                let vp = self.place(&args[1], Loc::Scratch(*s + 1));
                let cp = self.place(&args[2], Loc::Scratch(*s + 2));
                let dest = cvt_i32(self.read(dp) + 0.0001);
                let count = cvt_i32(self.read(cp) + 0.0001);
                let v = self.read(vp);
                self.mem.fill(dest, count, v);
                dp
            }
            // Plain value producers write into the caller-supplied cell.
            Node::Const(_) | Node::Un(..) | Node::Bin { .. } | Node::And(_) | Node::Or(_) => {
                let v = self.val(n);
                self.write(out, v);
                out
            }
        }
    }
}
