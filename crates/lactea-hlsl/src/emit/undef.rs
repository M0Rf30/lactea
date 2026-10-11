//! Reads of never-written variables, as projectM's GLSL path compiles them.
//!
//! HLSL leaves a variable that is read before it is written undefined, and projectM's generated GLSL does too
//! (a global or local without initialiser). Mesa's `nir_opt_undef` (`replace_ssa_undef`) then replaces an undefined
//! value that is used **only by float ALU instructions** with a NaN constant ("which will cause the opcode to be
//! eliminated by nir_opt_algebraic"): `cos(42 * undef)` is NaN, `max(x, NaN)` is `x`, `saturate(NaN)` is 0, ...
//! An undefined value with any other use (a vector constructor / texture coordinate, a `mov` into a store or phi)
//! stays `undef` and reads whatever the register held, which cannot be reproduced and keeps the WGSL default
//! of 0. For example, a never-assigned `uv3` in `uv3 = .4 * cos(42*uv3) + 64*dz;` leaves no trace in the final NIR,
//! so the term must evaluate as if absent: initialising `uv3` to NaN reproduces projectM's output, the default 0
//! does not.
//!
//! This models one driver's optimiser (Mesa) because projectM's reference output depends on it; it is only
//! active with `Options::projectm_flavor`. The analysis is deliberately conservative: it walks the `PS` body in
//! source order and treats a variable as written after its first (even partial, even conditional) assignment, and
//! every variable written inside a loop as written at loop entry; only variables whose reads *before that first
//! write* are all direct operands of float arithmetic / math intrinsics are returned.

use super::*;

/// Math intrinsics that become plain float ALU instructions in NIR (a `tex2D` coordinate, `float2(...)` constructor
/// or user-function argument does not: those are `vecN` / non-ALU uses that keep the undef).
fn is_alu_intrinsic(name: &str) -> bool {
    matches!(
        name,
        "abs"
            | "sign"
            | "acos"
            | "asin"
            | "atan"
            | "atan2"
            | "ceil"
            | "cos"
            | "cosh"
            | "exp"
            | "exp2"
            | "floor"
            | "log"
            | "log2"
            | "log10"
            | "sin"
            | "sinh"
            | "sqrt"
            | "tan"
            | "tanh"
            | "trunc"
            | "saturate"
            | "frac"
            | "rsqrt"
            | "round"
            | "pow"
            | "fmod"
            | "step"
            | "min"
            | "max"
            | "lerp"
            | "smoothstep"
            | "clamp"
            | "mad"
            | "length"
            | "normalize"
            | "distance"
            | "dot"
            | "cross"
            | "mul"
    )
}

fn alu_binop(op: BinOp) -> bool {
    use BinOp::*;
    matches!(
        op,
        Add | Sub | Mul | Div | Rem | Lt | Gt | Le | Ge | Eq | Ne
    )
}

struct An<'a> {
    tracked: HashSet<String>,
    written: HashSet<String>,
    /// variables read before their first write
    seen: HashSet<String>,
    /// ... and read at least once in a non-ALU context
    kept: HashSet<String>,
    funcs: &'a HashSet<String>,
}

/// The base identifier of an lvalue (`a`, `a.xy`, `a[i]`, `a[i].x`).
fn lvalue_base(e: &Expr) -> Option<&str> {
    match e {
        Expr::Ident(n) => Some(n),
        Expr::Member(b, _) | Expr::Index(b, _) => lvalue_base(b),
        _ => None,
    }
}

impl An<'_> {
    fn read(&mut self, name: &str, alu: bool) {
        if self.tracked.contains(name) && !self.written.contains(name) {
            self.seen.insert(name.to_string());
            if !alu {
                self.kept.insert(name.to_string());
            }
        }
    }

    fn write(&mut self, e: &Expr) {
        if let Some(n) = lvalue_base(e) {
            self.written.insert(n.to_string());
        }
    }

    /// Evaluate the index expressions inside an lvalue (they are reads in non-ALU context).
    fn lvalue_indices(&mut self, e: &Expr) {
        match e {
            Expr::Member(b, _) => self.lvalue_indices(b),
            Expr::Index(b, i) => {
                self.lvalue_indices(b);
                self.expr(i, false);
            }
            _ => {}
        }
    }

    /// `alu`: the value of `e` is consumed directly as a float operand of an ALU instruction.
    fn expr(&mut self, e: &Expr, alu: bool) {
        match e {
            Expr::Float(_) | Expr::Int(_) | Expr::Bool(_) => {}
            Expr::Ident(n) => self.read(n, alu),
            Expr::Unary(op, a) => match op {
                UnOp::Neg => self.expr(a, true),
                UnOp::Plus => self.expr(a, alu),
                _ => self.expr(a, false),
            },
            Expr::Binary(op, a, b) => {
                let c = alu_binop(*op);
                self.expr(a, c);
                self.expr(b, c);
            }
            Expr::Assign(op, l, r) => {
                self.expr(r, op.is_some_and(alu_binop));
                self.lvalue_indices(l);
                if op.is_some() {
                    self.expr(l, op.is_some_and(alu_binop));
                }
                self.write(l);
            }
            Expr::IncDec { e, .. } => {
                self.expr(e, true);
                self.write(e);
            }
            Expr::Cond(c, a, b) => {
                self.expr(c, false);
                self.expr(a, false);
                self.expr(b, false);
            }
            Expr::Comma(v) => {
                for x in v {
                    self.expr(x, false);
                }
            }
            Expr::Call(name, args) => {
                let user = self.funcs.contains(name);
                let alu = !user && is_alu_intrinsic(name);
                for a in args {
                    self.expr(a, alu);
                }
                if user {
                    // `out` / `inout` parameters write their argument
                    for a in args {
                        self.write(a);
                    }
                }
            }
            Expr::Ctor(_, args) => {
                for a in args {
                    self.expr(a, false);
                }
            }
            Expr::Cast(_, a) => self.expr(a, false),
            Expr::Index(b, i) => {
                self.expr(b, alu);
                self.expr(i, false);
            }
            Expr::Member(b, _) => self.expr(b, alu),
            Expr::InitList(v) => {
                for x in v {
                    self.expr(x, false);
                }
            }
        }
    }

    /// Mark every variable assigned anywhere in `s` (and in `extra`) as written: a loop-carried read is a phi.
    fn mark_loop_writes(&mut self, s: &Stmt, extra: &[&Expr]) {
        fn walk_expr(e: &Expr, out: &mut Vec<String>) {
            match e {
                Expr::Assign(_, l, r) => {
                    if let Some(n) = lvalue_base(l) {
                        out.push(n.to_string());
                    }
                    walk_expr(l, out);
                    walk_expr(r, out);
                }
                Expr::IncDec { e, .. } => {
                    if let Some(n) = lvalue_base(e) {
                        out.push(n.to_string());
                    }
                }
                Expr::Unary(_, a) | Expr::Cast(_, a) | Expr::Member(a, _) => walk_expr(a, out),
                Expr::Binary(_, a, b) | Expr::Index(a, b) => {
                    walk_expr(a, out);
                    walk_expr(b, out);
                }
                Expr::Cond(a, b, c) => {
                    walk_expr(a, out);
                    walk_expr(b, out);
                    walk_expr(c, out);
                }
                Expr::Comma(v) | Expr::InitList(v) | Expr::Ctor(_, v) => {
                    v.iter().for_each(|x| walk_expr(x, out))
                }
                Expr::Call(_, v) => v.iter().for_each(|x| {
                    // an argument of a call may be an `out` parameter
                    if let Some(n) = lvalue_base(x) {
                        out.push(n.to_string());
                    }
                    walk_expr(x, out)
                }),
                _ => {}
            }
        }
        fn walk_stmt(s: &Stmt, out: &mut Vec<String>) {
            match s {
                Stmt::Block(v) => v.iter().for_each(|x| walk_stmt(x, out)),
                Stmt::Decl(ds) => {
                    for d in ds {
                        out.push(d.name.clone());
                        if let Some(i) = &d.init {
                            walk_expr(i, out);
                        }
                    }
                }
                Stmt::Expr(e) => walk_expr(e, out),
                Stmt::If(c, a, b) => {
                    walk_expr(c, out);
                    walk_stmt(a, out);
                    if let Some(b) = b {
                        walk_stmt(b, out);
                    }
                }
                Stmt::For(i, c, st, b) => {
                    if let Some(i) = i {
                        walk_stmt(i, out);
                    }
                    if let Some(c) = c {
                        walk_expr(c, out);
                    }
                    if let Some(st) = st {
                        walk_expr(st, out);
                    }
                    walk_stmt(b, out);
                }
                Stmt::While(c, b) | Stmt::DoWhile(b, c) => {
                    walk_expr(c, out);
                    walk_stmt(b, out);
                }
                Stmt::Return(Some(e)) => walk_expr(e, out),
                _ => {}
            }
        }
        let mut names = Vec::new();
        walk_stmt(s, &mut names);
        for e in extra {
            walk_expr(e, &mut names);
        }
        self.written.extend(names);
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Empty | Stmt::Break | Stmt::Continue | Stmt::Discard => {}
            Stmt::Block(v) => v.iter().for_each(|x| self.stmt(x)),
            Stmt::Decl(ds) => {
                for d in ds {
                    if let Some(i) = &d.init {
                        self.expr(i, false);
                        self.written.insert(d.name.clone());
                    }
                }
            }
            Stmt::Expr(e) => self.expr(e, false),
            Stmt::If(c, a, b) => {
                self.expr(c, false);
                self.stmt(a);
                if let Some(b) = b {
                    self.stmt(b);
                }
            }
            Stmt::For(init, cond, step, body) => {
                if let Some(i) = init {
                    self.stmt(i);
                }
                let extra: Vec<&Expr> = cond.iter().chain(step.iter()).collect();
                self.mark_loop_writes(body, &extra);
                if let Some(c) = cond {
                    self.expr(c, false);
                }
                self.stmt(body);
                if let Some(st) = step {
                    self.expr(st, false);
                }
            }
            Stmt::While(c, body) => {
                self.mark_loop_writes(body, &[c]);
                self.expr(c, false);
                self.stmt(body);
            }
            Stmt::DoWhile(body, c) => {
                self.mark_loop_writes(body, &[c]);
                self.stmt(body);
                self.expr(c, false);
            }
            Stmt::Return(e) => {
                if let Some(e) = e {
                    self.expr(e, false);
                }
            }
        }
    }
}

/// Names of the variables (PS locals declared without initialiser and private globals) that Mesa turns into NaN.
pub(super) fn nan_vars(
    prog: &Program,
    ps: &Func,
    globals: &HashMap<String, GInfo>,
) -> HashSet<String> {
    // every local declaration of `PS` (name, type, has no initialiser)
    fn decls(s: &Stmt, out: &mut Vec<(String, Ty, bool)>) {
        match s {
            Stmt::Block(v) => v.iter().for_each(|x| decls(x, out)),
            Stmt::Decl(ds) => ds
                .iter()
                .for_each(|d| out.push((d.name.clone(), d.ty.clone(), d.init.is_none()))),
            Stmt::If(_, a, b) => {
                decls(a, out);
                if let Some(b) = b {
                    decls(b, out);
                }
            }
            Stmt::For(i, _, _, b) => {
                if let Some(i) = i {
                    decls(i, out);
                }
                decls(b, out);
            }
            Stmt::While(_, b) | Stmt::DoWhile(b, _) => decls(b, out),
            _ => {}
        }
    }
    let float_like = |t: &Ty| matches!(t, Ty::Sc(Sc::F | Sc::I) | Ty::Vec(Sc::F | Sc::I, _));
    let mut all = Vec::new();
    for s in &ps.body {
        decls(s, &mut all);
    }
    let mut tracked = HashSet::new();
    // locals declared without initialiser exactly once and not shadowing a global or a parameter
    for (name, ty, uninit) in &all {
        if *uninit
            && float_like(ty)
            && all.iter().filter(|(n, _, _)| n == name).count() == 1
            && !globals.contains_key(name)
            && !ps.params.iter().any(|p| &p.name == name)
        {
            tracked.insert(name.clone());
        }
    }
    for (name, g) in globals {
        if matches!(g.kind, GKind::Private)
            && g.decl.init.is_none()
            && g.shadow.is_none()
            && float_like(&g.decl.ty)
        {
            tracked.insert(name.clone());
        }
    }
    let funcs: HashSet<String> = prog.funcs.iter().map(|f| f.name.clone()).collect();
    let mut an = An {
        tracked,
        written: HashSet::new(),
        seen: HashSet::new(),
        kept: HashSet::new(),
        funcs: &funcs,
    };
    for s in &ps.body {
        an.stmt(s);
    }
    an.seen.difference(&an.kept).cloned().collect()
}
