//! Value-semantics fast tree.
//!
//! The exact evaluator in `eval.rs` models projectm-eval's reference passing (every function
//! returns a *pointer* that callers may write through).  That is needed for a handful of exotic
//! constructs only (`if(c,a,b) = x`, `exec3` out-pointer sharing, `while` conditions that mix
//! references and values, deferred operand reads over references, ...).  Everything else has
//! plain value semantics, so after compilation the exact tree is *lowered* into this tree:
//! leaf operands are inlined (`Opd`), common operators get dedicated nodes, assignments to
//! variables / megabuf cells write directly.  Any construct that is not provably
//! value-semantic is kept as a `Slow` node holding the exact tree, which is evaluated by the
//! exact interpreter, so semantics are preserved for the whole language.

use crate::context::Context;
use crate::eval::{CLOSE, MAX_LOOP, assign_op, b2f, bin, cvt_i64, resolve_mem};
use crate::ns;
use crate::tree::{AssignOp, BinOp, Loc, Node, UnOp};

/// Operand: constants and local variables are inlined (no recursive call).
#[derive(Debug)]
pub(crate) enum Opd {
    C(f64),
    V(u32),
    N(Box<FNode>),
}

#[derive(Debug)]
pub(crate) enum FNode {
    Const(f64),
    Var(u32),
    Reg(u8),
    Un(UnOp, Opd),
    Sin(Opd),
    Cos(Opd),
    Sqr(Opd),
    Sqrt(Opd),
    Abs(Opd),
    Neg(Opd),
    Bnot(Opd),
    Floor(Opd),
    Bin(BinOp, Opd, Opd),
    Add(Opd, Opd),
    Sub(Opd, Opd),
    Mul(Opd, Opd),
    Div(Opd, Opd),
    Above(Opd, Opd),
    Below(Opd, Opd),
    Equal(Opd, Opd),
    /// `a op b` where `a` is a local variable and `b` may modify it: `a` is read after `b`.
    BinDeferVar(BinOp, u32, Opd),
    BinDeferReg(BinOp, u8, Opd),
    /// The trailing `bool` of the control nodes is the ns-eel2 flag (see the matching `Node`).
    And(Opd, Opd, bool),
    Or(Opd, Opd, bool),
    If(Opd, Opd, Opd, bool),
    Exec2(Opd, Opd),
    List(Box<[FNode]>, Opd),
    Loop(Opd, Opd, bool),
    While(Opd, bool),
    SetVar(u32, Opd),
    /// ns-eel2 `=` to a variable: the stored value is filtered.
    SetVarF(u32, Opd),
    /// `filter`: ns-eel2 `=`.
    SetReg(u8, Opd, bool),
    SetMem {
        global: bool,
        ns: bool,
        filter: bool,
        idx: Opd,
        rhs: Opd,
    },
    OpVar(AssignOp, u32, Opd),
    OpReg(AssignOp, u8, Opd),
    OpMem {
        op: AssignOp,
        global: bool,
        ns: bool,
        idx: Opd,
        rhs: Opd,
    },
    MemGet {
        global: bool,
        ns: bool,
        idx: Opd,
    },
    /// Exact-semantics subtree.
    Slow(Box<Node>),
}

// ---------------------------------------------------------------------------------------
// lowering
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Out {
    /// Always writes its plain result through the incoming out cell.
    Writer,
    /// Never writes through the incoming out cell (always returns its own place).
    Keeper,
    Mixed,
}

fn merge(a: Out, b: Out) -> Out {
    if a == b { a } else { Out::Mixed }
}

/// How a node treats the out-pointer it is handed (only matters for `while`, whose C
/// implementation keeps one out-pointer across iterations).
fn out_kind(n: &Node) -> Out {
    match n {
        Node::Const(_) | Node::Un(..) | Node::Bin { .. } | Node::And(..) | Node::Or(..) => {
            Out::Writer
        }
        Node::Var(_) | Node::List { .. } | Node::Loop { .. } | Node::While { .. } => Out::Keeper,
        Node::Memcpy { .. } | Node::Memset { .. } => Out::Keeper,
        Node::If { args, .. } => merge(out_kind(&args[1]), out_kind(&args[2])),
        Node::Exec2 { args, .. } => out_kind(&args[1]),
        Node::Exec3 { args, .. } => out_kind(&args[2]),
        Node::Set { args, .. } | Node::OpAssign { args, .. } => out_kind(&args[0]),
        Node::FreeMbuf { arg } => out_kind(arg),
        Node::Mem { .. } => Out::Mixed,
    }
}

fn is_simple_var(n: &Node) -> bool {
    matches!(n, Node::Var(Loc::Var(_)) | Node::Var(Loc::Reg(_)))
}

fn lowerable(n: &Node) -> bool {
    match n {
        Node::Const(_)
        | Node::Un(..)
        | Node::And(..)
        | Node::Or(..)
        | Node::If { .. }
        | Node::Exec2 { .. }
        | Node::List { .. }
        | Node::Loop { .. }
        | Node::Mem { .. }
        | Node::FreeMbuf { .. } => true,
        Node::Var(l) => matches!(l, Loc::Var(_) | Loc::Reg(_)),
        Node::Bin { defer, args, .. } => !*defer || is_simple_var(&args[0]),
        Node::While { arg, .. } => out_kind(arg) != Out::Mixed,
        Node::Set { args, .. } | Node::OpAssign { args, .. } => {
            is_simple_var(&args[0]) || matches!(args[0], Node::Mem { .. })
        }
        Node::Exec3 { .. } | Node::Memcpy { .. } | Node::Memset { .. } => false,
    }
}

fn to_opd(f: FNode) -> Opd {
    match f {
        FNode::Const(c) => Opd::C(c),
        FNode::Var(i) => Opd::V(i),
        f => Opd::N(Box::new(f)),
    }
}

fn opd(n: Node) -> Opd {
    to_opd(lower(n))
}

pub(crate) fn lower(n: Node) -> FNode {
    if !lowerable(&n) {
        return FNode::Slow(Box::new(n));
    }
    match n {
        Node::Const(c) => FNode::Const(c),
        Node::Var(Loc::Var(i)) => FNode::Var(i),
        Node::Var(Loc::Reg(i)) => FNode::Reg(i),
        Node::Var(_) => unreachable!(),
        Node::Un(op, a) => {
            let a = opd(*a);
            match op {
                UnOp::Sin => FNode::Sin(a),
                UnOp::Cos => FNode::Cos(a),
                UnOp::Sqr => FNode::Sqr(a),
                UnOp::Sqrt => FNode::Sqrt(a),
                UnOp::Abs => FNode::Abs(a),
                UnOp::Neg => FNode::Neg(a),
                UnOp::Bnot => FNode::Bnot(a),
                UnOp::Floor => FNode::Floor(a),
                op => FNode::Un(op, a),
            }
        }
        Node::Bin { op, args, defer } => {
            let [a, b] = *args;
            if defer {
                let b = opd(b);
                return match a {
                    Node::Var(Loc::Var(i)) => FNode::BinDeferVar(op, i, b),
                    Node::Var(Loc::Reg(i)) => FNode::BinDeferReg(op, i, b),
                    _ => unreachable!("checked by lowerable"),
                };
            }
            let (a, b) = (opd(a), opd(b));
            match op {
                BinOp::Add => FNode::Add(a, b),
                BinOp::Sub => FNode::Sub(a, b),
                BinOp::Mul => FNode::Mul(a, b),
                BinOp::Div => FNode::Div(a, b),
                BinOp::Above => FNode::Above(a, b),
                BinOp::Below => FNode::Below(a, b),
                BinOp::Equal => FNode::Equal(a, b),
                op => FNode::Bin(op, a, b),
            }
        }
        Node::And(args, ns) => {
            let [a, b] = *args;
            FNode::And(opd(a), opd(b), ns)
        }
        Node::Or(args, ns) => {
            let [a, b] = *args;
            FNode::Or(opd(a), opd(b), ns)
        }
        Node::If { args, ns, .. } => {
            let [c, t, f] = *args;
            FNode::If(opd(c), opd(t), opd(f), ns)
        }
        Node::Exec2 { args, .. } => {
            let [a, b] = *args;
            FNode::Exec2(opd(a), opd(b))
        }
        Node::List { items, .. } => {
            let mut v: Vec<FNode> = items.into_iter().map(lower).collect();
            let last = v.pop().expect("non-empty list");
            FNode::List(v.into_boxed_slice(), to_opd(last))
        }
        Node::Loop { args, ns, .. } => {
            let [c, b] = *args;
            FNode::Loop(opd(c), opd(b), ns)
        }
        Node::While { arg, ns, .. } => FNode::While(opd(*arg), ns),
        Node::Set { args, filter, .. } => {
            let [l, r] = *args;
            let rhs = opd(r);
            match l {
                Node::Var(Loc::Var(i)) if filter => FNode::SetVarF(i, rhs),
                Node::Var(Loc::Var(i)) => FNode::SetVar(i, rhs),
                Node::Var(Loc::Reg(i)) => FNode::SetReg(i, rhs, filter),
                Node::Mem {
                    global, ns, arg, ..
                } => FNode::SetMem {
                    global,
                    ns,
                    filter,
                    idx: opd(*arg),
                    rhs,
                },
                _ => unreachable!("checked by lowerable"),
            }
        }
        Node::OpAssign { op, args, .. } => {
            let [l, r] = *args;
            let rhs = opd(r);
            match l {
                Node::Var(Loc::Var(i)) => FNode::OpVar(op, i, rhs),
                Node::Var(Loc::Reg(i)) => FNode::OpReg(op, i, rhs),
                Node::Mem {
                    global, ns, arg, ..
                } => FNode::OpMem {
                    op,
                    global,
                    ns,
                    idx: opd(*arg),
                    rhs,
                },
                _ => unreachable!("checked by lowerable"),
            }
        }
        Node::Mem {
            global, ns, arg, ..
        } => FNode::MemGet {
            global,
            ns,
            idx: opd(*arg),
        },
        Node::FreeMbuf { arg } => lower(*arg),
        Node::Exec3 { .. } | Node::Memcpy { .. } | Node::Memset { .. } => unreachable!(),
    }
}

// ---------------------------------------------------------------------------------------
// evaluation
// ---------------------------------------------------------------------------------------

// Invariant for every `Var(i)` / `V(i)` / `SetVar(i, _)` slot below: `i < self.vars.len()`.
// Slots come from `Context::var` (which pushes the slot before handing it out) and
// `Context::vars` never shrinks, so the unchecked `self.vars[i]` cannot go out of range for
// a `Program` run by the context that compiled it (enforced by the `ctx_id` assert in `run`).
impl Context {
    #[inline(always)]
    fn opd(&mut self, o: &Opd) -> f64 {
        match o {
            Opd::C(c) => *c,
            Opd::V(i) => self.vars[*i as usize],
            Opd::N(n) => self.fval(n),
        }
    }

    #[inline(always)]
    fn mem_read(&self, global: bool, i: u32) -> f64 {
        if global {
            self.global.gmem_get(i)
        } else {
            self.mem.get(i)
        }
    }

    #[inline(always)]
    fn mem_write(&mut self, global: bool, i: u32, v: f64) {
        if global {
            self.write(Loc::GMem(i), v);
        } else {
            self.mem.set(i, v);
        }
    }

    pub(crate) fn fval(&mut self, n: &FNode) -> f64 {
        match n {
            FNode::Const(c) => *c,
            FNode::Var(i) => self.vars[*i as usize],
            FNode::Sin(a) => self.opd(a).sin(),
            FNode::Cos(a) => self.opd(a).cos(),
            FNode::Add(a, b) => {
                let x = self.opd(a);
                x + self.opd(b)
            }
            FNode::Sub(a, b) => {
                let x = self.opd(a);
                x - self.opd(b)
            }
            FNode::Mul(a, b) => {
                let x = self.opd(a);
                x * self.opd(b)
            }
            FNode::SetVar(i, rhs) => {
                let v = self.opd(rhs);
                self.vars[*i as usize] = v;
                v
            }
            FNode::SetVarF(i, rhs) => {
                let v = ns::assign_filter(self.opd(rhs));
                self.vars[*i as usize] = v;
                v
            }
            _ => self.fval_cold(n),
        }
    }

    #[inline(never)]
    fn fval_cold(&mut self, n: &FNode) -> f64 {
        match n {
            FNode::Reg(i) => self.global.reg(*i as usize),
            FNode::Sqr(a) => {
                let x = self.opd(a);
                x * x
            }
            FNode::Sqrt(a) => self.opd(a).abs().sqrt(),
            FNode::Abs(a) => self.opd(a).abs(),
            FNode::Neg(a) => -self.opd(a),
            FNode::Bnot(a) => b2f(self.opd(a).abs() < CLOSE),
            FNode::Floor(a) => self.opd(a).floor(),
            FNode::Un(op, a) => {
                let x = self.opd(a);
                self.un(*op, x)
            }
            FNode::Div(a, b) => {
                let x = self.opd(a);
                let y = self.opd(b);
                if y.abs() < CLOSE { 0.0 } else { x / y }
            }
            FNode::Above(a, b) => {
                let x = self.opd(a);
                b2f(x > self.opd(b))
            }
            FNode::Below(a, b) => {
                let x = self.opd(a);
                b2f(x < self.opd(b))
            }
            FNode::Equal(a, b) => {
                let x = self.opd(a);
                let y = self.opd(b);
                b2f((x - y).abs() < CLOSE)
            }
            FNode::Bin(op, a, b) => {
                let x = self.opd(a);
                let y = self.opd(b);
                bin(*op, x, y)
            }
            FNode::BinDeferVar(op, i, b) => {
                let y = self.opd(b);
                bin(*op, self.vars[*i as usize], y)
            }
            FNode::BinDeferReg(op, i, b) => {
                let y = self.opd(b);
                bin(*op, self.global.reg(*i as usize), y)
            }
            FNode::And(a, b, ns) => {
                if *ns {
                    if ns::truthy(self.opd(a)) {
                        b2f(ns::truthy(self.opd(b)))
                    } else {
                        0.0
                    }
                } else if self.opd(a).abs() > CLOSE {
                    b2f(self.opd(b).abs() > CLOSE)
                } else {
                    0.0
                }
            }
            FNode::Or(a, b, ns) => {
                if *ns {
                    if ns::truthy(self.opd(a)) {
                        1.0
                    } else {
                        b2f(ns::truthy(self.opd(b)))
                    }
                } else if self.opd(a).abs() < CLOSE {
                    b2f(self.opd(b).abs() > CLOSE)
                } else {
                    1.0
                }
            }
            FNode::If(c, t, f, ns) => {
                let x = self.opd(c);
                if if *ns { ns::truthy(x) } else { x != 0.0 } {
                    self.opd(t)
                } else {
                    self.opd(f)
                }
            }
            FNode::Exec2(a, b) => {
                self.opd(a);
                self.opd(b)
            }
            FNode::List(stmts, last) => {
                for s in stmts.iter() {
                    self.fval(s);
                }
                self.opd(last)
            }
            FNode::Loop(count, body, ns) => {
                let mut last = self.opd(count);
                let mut n = if *ns {
                    i64::from(ns::fistp32(last))
                } else {
                    cvt_i64(last)
                };
                if n > MAX_LOOP {
                    n = MAX_LOOP;
                }
                let mut i = 0;
                while i < n {
                    last = self.opd(body);
                    i += 1;
                }
                last
            }
            FNode::While(cond, ns) => {
                let mut left = MAX_LOOP;
                loop {
                    let v = self.opd(cond);
                    if if *ns { ns::truthy(v) } else { v.abs() > CLOSE } {
                        left -= 1;
                        if left != 0 {
                            continue;
                        }
                    }
                    return v;
                }
            }
            FNode::SetReg(i, rhs, filter) => {
                let mut v = self.opd(rhs);
                if *filter {
                    v = ns::assign_filter(v);
                }
                self.global.set_reg(*i as usize, v);
                v
            }
            FNode::SetMem {
                global,
                ns,
                filter,
                idx,
                rhs,
            } => {
                let k = resolve_mem(self.opd(idx), *global, *ns);
                let mut v = self.opd(rhs);
                if *filter {
                    v = ns::assign_filter(v);
                }
                if let Some(k) = k {
                    self.mem_write(*global, k, v);
                }
                v
            }
            FNode::OpVar(op, i, rhs) => {
                let r = self.opd(rhs);
                let v = assign_op(*op, self.vars[*i as usize], r);
                self.vars[*i as usize] = v;
                v
            }
            FNode::OpReg(op, i, rhs) => {
                let r = self.opd(rhs);
                let v = assign_op(*op, self.global.reg(*i as usize), r);
                self.global.set_reg(*i as usize, v);
                v
            }
            FNode::OpMem {
                op,
                global,
                ns,
                idx,
                rhs,
            } => {
                let k = resolve_mem(self.opd(idx), *global, *ns);
                let r = self.opd(rhs);
                match k {
                    Some(k) => {
                        let v = assign_op(*op, self.mem_read(*global, k), r);
                        self.mem_write(*global, k, v);
                        v
                    }
                    // out of range: the cell is a scratch cell that was just set to 0
                    None => assign_op(*op, 0.0, r),
                }
            }
            FNode::MemGet { global, ns, idx } => match resolve_mem(self.opd(idx), *global, *ns) {
                Some(k) => self.mem_read(*global, k),
                None => 0.0,
            },
            FNode::Slow(n) => self.val(n),
            _ => unreachable!(),
        }
    }
}
