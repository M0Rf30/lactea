//! Mesa's backward inter-shader code motion, as projectM's GL path goes through it.
//!
//! `nir_opt_varyings` (the linker pass of Mesa 26.2, run by the iris driver) moves arithmetic on interpolated fragment
//! inputs into the vertex shader: `interp(x) + c` becomes `interp(x + c)` (`c` constant or uniform, "convergent"), the
//! sum is computed per vertex and the rasteriser interpolates it. wgpu (ANV) interpolates `x` and adds in the fragment
//! stage; both differ by about one ulp, which a division by a small value or a feedback loop amplifies.
//!
//! The pass moves, for every fragment input load `L`, the farthest instruction that **post-dominates** `L` in the SSA
//! use graph and is movable (`nir_use_dominance.c`: an instruction post-dominates another when every use path of the
//! latter ends in it; stores and `if` conditions end at a dummy root). Movable are: a constant or uniform expression
//! (any ALU op over convergent values), and `fadd` / `fsub` / `mov` (never `fneg`: measured), `fdiv` by a convergent
//! non-constant, `fmul` / `ffma` with one convergent source, `flrp` with a convergent weight, over interpolated values
//! of one interpolation mode. HLSL's `a * b` is `mult(a, b)` in projectM (a `select` on `a == 0`), which is never
//! movable; `*=` and `mul()` are plain products. A load that has an unmovable use, or uses that do not converge on
//! one movable instruction (a warp shader's `uv` also feeds the motion-vector output), starts no motion; it can still be
//! an input of the expression another load starts.
//!
//! This module mirrors that decision on the HLSL AST of `PS`: it builds the scalar SSA use graph (value numbering as
//! NIR's CSE does, dead code removed), finds the instructions Mesa moves and returns, per source expression, the
//! components whose value the fragment stage must read from an extra varying, plus the vertex-stage function that
//! computes each. It is only active with `Options::gl_varying_fold` and is conservative: anything it does not model
//! (user functions, arrays, matrices, `?:`, comparisons, ...) is an unmovable use, which leaves the expression as it is.

use super::expr::parse_swizzle;
use super::*;
use lactea_abi::PassKind;

/// Extra varyings the vertex stage may carry (4 per `vec4` location).
const MAX_FOLDS: usize = 16;

type N = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    /// Constant / uniform expression.
    Conv,
    /// Interpolated and movable.
    Interp,
    /// Anything else.
    Unmov,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Input {
    /// `_uv`: xy = `uv`, zw = `uv_orig`.
    Uv,
    RadAng,
    Diffuse,
}

#[derive(Clone, Debug)]
enum Kind {
    Conv,
    Load(Input, usize),
    Add(N, N),
    Sub(N, N),
    Mul(N, N),
    Div(N, N),
    Lerp(N, N, N),
    /// `fneg` of an interpolated value, which is never moved.
    Neg(N),
    /// Replaced by the interpolated output of a fold.
    Folded,
    Other,
}

struct Node {
    kind: Kind,
    class: Class,
    /// `Conv`: the scalar expression (free of locals).
    ast: Option<Expr>,
    srcs: Vec<N>,
    uses: Vec<N>,
    /// Feeds a store, an `if` condition or a discard.
    exit: bool,
    live: bool,
    /// Source expressions (address, component) that compute this value.
    sites: Vec<(usize, usize)>,
}

/// A value: one node per component, or one node standing for an aggregate of unknown shape.
#[derive(Clone)]
enum Val {
    V(Vec<N>),
    Agg(N),
}

#[derive(Clone)]
struct Var {
    ty: Ty,
    val: Val,
}

type Env = Vec<HashMap<String, Var>>;

struct An<'t, 'a> {
    tr: &'t Tr<'a>,
    kind: PassKind,
    nodes: Vec<Node>,
    intern: HashMap<String, N>,
    env: Env,
}

impl<'t, 'a> An<'t, 'a> {
    fn push(&mut self, kind: Kind, class: Class, ast: Option<Expr>, srcs: Vec<N>) -> N {
        let n = self.nodes.len();
        for &s in &srcs {
            self.nodes[s].uses.push(n);
        }
        self.nodes.push(Node {
            kind,
            class,
            ast,
            srcs,
            uses: Vec::new(),
            exit: false,
            live: false,
            sites: Vec::new(),
        });
        n
    }

    /// A convergent scalar, interned by its expression.
    fn conv(&mut self, ast: Expr) -> N {
        let key = format!("c{ast:?}");
        if let Some(&n) = self.intern.get(&key) {
            return n;
        }
        let n = self.push(Kind::Conv, Class::Conv, Some(ast), Vec::new());
        self.intern.insert(key, n);
        n
    }

    /// An unmovable instruction consuming `srcs`.
    fn other(&mut self, srcs: Vec<N>) -> N {
        self.push(Kind::Other, Class::Unmov, None, srcs)
    }

    fn load(&mut self, input: Input, c: usize) -> N {
        let key = format!("l{input:?}{c}");
        if let Some(&n) = self.intern.get(&key) {
            return n;
        }
        let n = self.push(Kind::Load(input, c), Class::Interp, None, Vec::new());
        // projectM's warp shader copies `_uv.xy` to a second render target
        // (`_mv_tex_coords.xy = _uv.xy`), a use no movable instruction post-dominates
        if self.kind == PassKind::Warp && input == Input::Uv && c < 2 {
            self.nodes[n].exit = true;
        }
        self.intern.insert(key, n);
        n
    }

    fn class(&self, n: N) -> Class {
        self.nodes[n].class
    }

    fn ast(&self, n: N) -> Expr {
        self.nodes[n]
            .ast
            .clone()
            .expect("a convergent node has an expression")
    }

    fn is_literal(&self, n: N) -> bool {
        fn lit(e: &Expr) -> bool {
            match e {
                Expr::Float(_) | Expr::Int(_) => true,
                Expr::Unary(UnOp::Neg | UnOp::Plus, a) => lit(a),
                _ => false,
            }
        }
        self.nodes[n].ast.as_ref().is_some_and(lit)
    }

    /// A movable ALU node over interpolated values (value numbering: identical instructions are one).
    fn alu(&mut self, kind: Kind, srcs: Vec<N>, site: Option<(usize, usize)>) -> N {
        let key = match &kind {
            Kind::Add(a, b) => format!("+{},{}", a.min(b), a.max(b)),
            Kind::Sub(a, b) => format!("-{a},{b}"),
            Kind::Mul(a, b) => format!("*{},{}", a.min(b), a.max(b)),
            Kind::Div(a, b) => format!("/{a},{b}"),
            Kind::Lerp(a, b, t) => format!("m{a},{b},{t}"),
            _ => unreachable!("not an ALU kind"),
        };
        let n = match self.intern.get(&key) {
            Some(&n) => n,
            None => {
                let n = self.push(kind, Class::Interp, None, srcs);
                self.intern.insert(key, n);
                n
            }
        };
        if let Some(s) = site
            && !self.nodes[n].sites.contains(&s)
        {
            self.nodes[n].sites.push(s);
        }
        n
    }

    fn add_sub(&mut self, op: BinOp, a: N, b: N, site: Option<(usize, usize)>) -> N {
        let (ca, cb) = (self.class(a), self.class(b));
        if ca == Class::Unmov || cb == Class::Unmov {
            return self.other(vec![a, b]);
        }
        if ca == Class::Conv && cb == Class::Conv {
            let (x, y) = (self.ast(a), self.ast(b));
            return self.conv(Expr::Binary(op, Box::new(x), Box::new(y)));
        }
        let kind = if op == BinOp::Add {
            Kind::Add(a, b)
        } else {
            Kind::Sub(a, b)
        };
        self.alu(kind, vec![a, b], site)
    }

    /// `mult(a, b)`: movable only when both are convergent.
    fn mult(&mut self, a: N, b: N) -> N {
        if self.class(a) == Class::Conv && self.class(b) == Class::Conv {
            let (x, y) = (self.ast(a), self.ast(b));
            return self.conv(Expr::Binary(BinOp::Mul, Box::new(x), Box::new(y)));
        }
        self.other(vec![a, b])
    }

    /// A plain product (`*=`, `mul()`).
    fn plain_mul(&mut self, a: N, b: N, site: Option<(usize, usize)>) -> N {
        let (ca, cb) = (self.class(a), self.class(b));
        match (ca, cb) {
            (Class::Conv, Class::Conv) => self.mult(a, b),
            (Class::Interp, Class::Conv) | (Class::Conv, Class::Interp) => {
                self.alu(Kind::Mul(a, b), vec![a, b], site)
            }
            _ => self.other(vec![a, b]),
        }
    }

    fn div(&mut self, a: N, b: N, site: Option<(usize, usize)>) -> N {
        let (ca, cb) = (self.class(a), self.class(b));
        if ca == Class::Conv && cb == Class::Conv {
            let (x, y) = (self.ast(a), self.ast(b));
            return self.conv(Expr::Binary(BinOp::Div, Box::new(x), Box::new(y)));
        }
        // `x / constant` is `x * constant'` in GLSL, a product that is only movable fused into an `ffma`; modelled as unmovable
        if ca == Class::Interp && cb == Class::Conv && !self.is_literal(b) {
            return self.alu(Kind::Div(a, b), vec![a, b], site);
        }
        self.other(vec![a, b])
    }

    fn lerp(&mut self, a: N, b: N, t: N, site: Option<(usize, usize)>) -> N {
        let cls = [self.class(a), self.class(b), self.class(t)];
        if cls.contains(&Class::Unmov) {
            return self.other(vec![a, b, t]);
        }
        if cls.iter().all(|c| *c == Class::Conv) {
            let asts = vec![self.ast(a), self.ast(b), self.ast(t)];
            return self.conv(Expr::Call("lerp".into(), asts));
        }
        if cls[2] == Class::Conv || (cls[0] == Class::Conv && cls[1] == Class::Conv) {
            return self.alu(Kind::Lerp(a, b, t), vec![a, b, t], site);
        }
        self.other(vec![a, b, t])
    }

    fn neg(&mut self, a: N) -> N {
        match self.class(a) {
            Class::Conv => {
                let x = self.ast(a);
                self.conv(Expr::Unary(UnOp::Neg, Box::new(x)))
            }
            _ => {
                // `-(-x)` is `x`: NIR folds the double negation; a lone `fneg` of an interpolated value is not moved
                if let Kind::Neg(x) = self.nodes[a].kind {
                    return x;
                }
                self.push(Kind::Neg(a), Class::Unmov, None, vec![a])
            }
        }
    }

    // ---------- values ----------

    fn comps(&mut self, v: &Val) -> Vec<N> {
        match v {
            Val::V(c) => c.clone(),
            Val::Agg(n) => vec![*n],
        }
    }

    fn agg(&mut self, srcs: Vec<N>) -> Val {
        Val::Agg(self.other(srcs))
    }

    fn exit(&mut self, v: &Val) {
        for n in self.comps(v) {
            self.nodes[n].exit = true;
        }
    }

    /// `v` as a value of type `ty`.
    fn fit(&mut self, v: Val, ty: &Ty) -> Val {
        let d = ty.dim() as usize;
        if d == 0 {
            let s = self.comps(&v);
            return self.agg(s);
        }
        let mut c = match v {
            Val::V(c) => c,
            Val::Agg(n) => vec![n; d],
        };
        if c.len() == 1 && d > 1 {
            c = vec![c[0]; d];
        }
        c.truncate(d);
        while c.len() < d {
            let o = self.other(Vec::new());
            c.push(o);
        }
        if ty.sc() != Some(Sc::F) {
            // an integer / bool conversion (truncation, comparison result): not a linear operation
            for x in c.iter_mut() {
                if self.nodes[*x].class != Class::Conv || !self.is_literal(*x) {
                    *x = self.other(vec![*x]);
                }
            }
        }
        Val::V(c)
    }

    fn lookup(&self, name: &str) -> Option<&Var> {
        self.env.iter().rev().find_map(|s| s.get(name))
    }

    fn set_var(&mut self, name: &str, val: Val) {
        for s in self.env.iter_mut().rev() {
            if let Some(v) = s.get_mut(name) {
                v.val = val;
                return;
            }
        }
    }

    fn global(&mut self, name: &str) -> Val {
        let tr = self.tr;
        let Some(g) = tr.globals.get(name) else {
            return self.agg(Vec::new());
        };
        let numeric = matches!(g.decl.ty, Ty::Sc(_) | Ty::Vec(..));
        match g.kind {
            GKind::Mapped(_) | GKind::PassTexsize if numeric && g.shadow.is_none() => {
                let d = g.decl.ty.dim() as usize;
                if d == 1 {
                    Val::V(vec![self.conv(Expr::Ident(name.to_string()))])
                } else {
                    let c = (0..d)
                        .map(|i| {
                            self.conv(Expr::Member(
                                Box::new(Expr::Ident(name.to_string())),
                                "xyzw"[i..i + 1].to_string(),
                            ))
                        })
                        .collect();
                    Val::V(c)
                }
            }
            _ => self.agg(Vec::new()),
        }
    }

    // ---------- expressions ----------

    fn ev(&mut self, e: &Expr) -> Val {
        let site = e as *const Expr as usize;
        match e {
            Expr::Float(v) => {
                let v = if self.tr.pm_flavor {
                    round_literal_6(*v)
                } else {
                    *v
                };
                Val::V(vec![self.conv(Expr::Float(v))])
            }
            Expr::Int(v) => Val::V(vec![self.conv(Expr::Int(*v))]),
            Expr::Bool(_) => self.agg(Vec::new()),
            Expr::Ident(n) => {
                if let Some(v) = self.lookup(n) {
                    return v.val.clone();
                }
                self.global(n)
            }
            Expr::Unary(op, a) => {
                let v = self.ev(a);
                match op {
                    UnOp::Plus => v,
                    UnOp::Neg => match v {
                        Val::V(c) => Val::V(c.into_iter().map(|x| self.neg(x)).collect()),
                        Val::Agg(n) => self.agg(vec![n]),
                    },
                    _ => {
                        let s = self.comps(&v);
                        self.agg(s)
                    }
                }
            }
            Expr::Binary(op, a, b) => {
                let (va, vb) = (self.ev(a), self.ev(b));
                self.binary(*op, va, vb, site, false)
            }
            Expr::Assign(op, l, r) => {
                let rv = self.ev(r);
                let val = match op {
                    None => rv,
                    Some(o) => {
                        let cur = self.ev(l);
                        self.binary(*o, cur, rv, site, true)
                    }
                };
                self.assign(l, val)
            }
            Expr::IncDec { inc, e, .. } => {
                let cur = self.ev(e);
                let one = Val::V(vec![self.conv(Expr::Int(1))]);
                let op = if *inc { BinOp::Add } else { BinOp::Sub };
                let val = self.binary(op, cur, one, site, true);
                self.assign(e, val)
            }
            Expr::Cond(c, a, b) => {
                let mut s = Vec::new();
                for x in [c, a, b] {
                    let v = self.ev(x);
                    s.extend(self.comps(&v));
                }
                self.agg(s)
            }
            Expr::Comma(v) => {
                let mut last = self.agg(Vec::new());
                for x in v {
                    last = self.ev(x);
                }
                last
            }
            Expr::Call(name, args) => self.call(name, args, site),
            Expr::Ctor(ty, args) => {
                let mut c: Vec<N> = Vec::new();
                let mut agg = Vec::new();
                for a in args {
                    match self.ev(a) {
                        Val::V(v) => c.extend(v),
                        Val::Agg(n) => {
                            agg.push(n);
                            c.push(n);
                        }
                    }
                }
                if !agg.is_empty() || ty.dim() == 0 {
                    return self.agg(c);
                }
                self.fit(Val::V(c), ty)
            }
            Expr::Cast(ty, a) => {
                let v = self.ev(a);
                self.fit(v, ty)
            }
            Expr::Index(b, i) => {
                let mut s = Vec::new();
                for x in [b, i] {
                    let v = self.ev(x);
                    s.extend(self.comps(&v));
                }
                self.agg(s)
            }
            Expr::Member(b, m) => {
                let v = self.ev(b);
                match (v, parse_swizzle(m)) {
                    (Val::V(c), Some(idx)) if idx.iter().all(|&i| i < c.len()) => {
                        Val::V(idx.iter().map(|&i| c[i]).collect())
                    }
                    (v, _) => {
                        let s = self.comps(&v);
                        self.agg(s)
                    }
                }
            }
            Expr::InitList(v) => {
                let mut s = Vec::new();
                for x in v {
                    let w = self.ev(x);
                    s.extend(self.comps(&w));
                }
                self.agg(s)
            }
        }
    }

    fn binary(&mut self, op: BinOp, a: Val, b: Val, site: usize, plain: bool) -> Val {
        let (Val::V(ca), Val::V(cb)) = (&a, &b) else {
            let mut s = self.comps(&a);
            s.extend(self.comps(&b));
            return self.agg(s);
        };
        let n = if ca.len() == cb.len() {
            ca.len()
        } else if ca.len() == 1 {
            cb.len()
        } else if cb.len() == 1 {
            ca.len()
        } else {
            ca.len().min(cb.len())
        };
        let pick = |c: &Vec<N>, i: usize| if c.len() == 1 { c[0] } else { c[i] };
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let (x, y) = (pick(ca, i), pick(cb, i));
            let r = match op {
                BinOp::Add | BinOp::Sub => self.add_sub(op, x, y, Some((site, i))),
                BinOp::Div => self.div(x, y, Some((site, i))),
                BinOp::Mul if plain => self.plain_mul(x, y, Some((site, i))),
                BinOp::Mul => self.mult(x, y),
                _ => self.other(vec![x, y]),
            };
            out.push(r);
        }
        Val::V(out)
    }

    fn call(&mut self, name: &str, args: &[Expr], site: usize) -> Val {
        let vals: Vec<Val> = args.iter().map(|a| self.ev(a)).collect();
        if self.tr.fns.contains_key(name) {
            // arguments of a user function are unmovable uses; its out parameters are unknown values afterwards
            let mut s = Vec::new();
            for v in &vals {
                s.extend(self.comps(v));
            }
            for p in args {
                if let Some(root) = lvalue_root_expr(p)
                    && self.lookup(root).is_some()
                {
                    let ty = self.lookup(root).map(|v| v.ty.clone()).unwrap_or(Ty::F);
                    let o = self.agg(Vec::new());
                    let o = self.fit(o, &ty);
                    self.set_var(root, o);
                }
            }
            return self.agg(s);
        }
        let all_vec = vals.iter().all(|v| matches!(v, Val::V(_)));
        match (name, all_vec, vals.as_slice()) {
            ("lerp", true, [Val::V(a), Val::V(b), Val::V(t)]) => {
                let n = a.len().max(b.len()).max(t.len());
                let pick = |c: &Vec<N>, i: usize| {
                    if c.len() == 1 {
                        c[0]
                    } else {
                        c[i.min(c.len() - 1)]
                    }
                };
                let out = (0..n)
                    .map(|i| self.lerp(pick(a, i), pick(b, i), pick(t, i), Some((site, i))))
                    .collect();
                return Val::V(out);
            }
            ("mul", true, [Val::V(a), Val::V(b)]) if a.len() == 1 && b.len() == 1 => {
                return Val::V(vec![self.plain_mul(a[0], b[0], Some((site, 0)))]);
            }
            _ => {}
        }
        if all_vec && is_componentwise(name) && !vals.is_empty() {
            let cols: Vec<&Vec<N>> = vals
                .iter()
                .map(|v| match v {
                    Val::V(c) => c,
                    Val::Agg(_) => unreachable!("checked above"),
                })
                .collect();
            let n = cols.iter().map(|c| c.len()).max().unwrap_or(1);
            if cols
                .iter()
                .all(|c| c.iter().all(|&x| self.nodes[x].class == Class::Conv))
            {
                let out = (0..n)
                    .map(|i| {
                        let asts: Vec<Expr> = cols
                            .iter()
                            .map(|c| {
                                self.ast(if c.len() == 1 {
                                    c[0]
                                } else {
                                    c[i.min(c.len() - 1)]
                                })
                            })
                            .collect();
                        self.conv(Expr::Call(name.to_string(), asts))
                    })
                    .collect();
                return Val::V(out);
            }
        }
        let mut s = Vec::new();
        for v in &vals {
            s.extend(self.comps(v));
        }
        self.agg(s)
    }

    /// Store `val` into the lvalue `l`; returns the stored value.
    fn assign(&mut self, l: &Expr, val: Val) -> Val {
        match l {
            Expr::Ident(n) => {
                if let Some(ty) = self.lookup(n).map(|v| v.ty.clone()) {
                    let v = self.fit(val, &ty);
                    self.set_var(n, v.clone());
                    return v;
                }
                // an output parameter or a global: a store
                self.exit(&val);
                val
            }
            Expr::Member(b, m) => {
                let Some(root) = lvalue_root_expr(b) else {
                    self.exit(&val);
                    return val;
                };
                let root = root.to_string();
                let idx = parse_swizzle(m);
                let Some(var) = self.lookup(&root).cloned() else {
                    self.exit(&val);
                    return val;
                };
                // only `v.xyz = ...` on a plain variable keeps component identity
                if let (Expr::Ident(_), Some(idx), Val::V(cur)) = (&**b, idx, &var.val) {
                    let comps = self.comps(&val);
                    let mut cur = cur.clone();
                    for (k, &i) in idx.iter().enumerate() {
                        if i < cur.len() {
                            cur[i] = if comps.len() == 1 {
                                comps[0]
                            } else {
                                comps[k.min(comps.len() - 1)]
                            };
                        }
                    }
                    self.set_var(&root, Val::V(cur));
                    return val;
                }
                self.exit(&val);
                let o = self.agg(Vec::new());
                let o = self.fit(o, &var.ty);
                self.set_var(&root, o);
                val
            }
            _ => {
                self.exit(&val);
                if let Some(root) = lvalue_root_expr(l).map(str::to_string)
                    && let Some(ty) = self.lookup(&root).map(|v| v.ty.clone())
                {
                    let o = self.agg(Vec::new());
                    let o = self.fit(o, &ty);
                    self.set_var(&root, o);
                }
                val
            }
        }
    }

    // ---------- statements ----------

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Empty | Stmt::Break | Stmt::Continue => {}
            Stmt::Discard => {}
            Stmt::Block(v) => {
                self.env.push(HashMap::new());
                for x in v {
                    self.stmt(x);
                }
                self.env.pop();
            }
            Stmt::Decl(ds) => {
                for d in ds {
                    let val = match &d.init {
                        Some(i) => {
                            let v = self.ev(i);
                            self.fit(v, &d.ty)
                        }
                        None => {
                            let o = self.agg(Vec::new());
                            self.fit(o, &d.ty)
                        }
                    };
                    if let Some(sc) = self.env.last_mut() {
                        sc.insert(
                            d.name.clone(),
                            Var {
                                ty: d.ty.clone(),
                                val,
                            },
                        );
                    }
                }
            }
            Stmt::Expr(e) => {
                self.ev(e);
            }
            Stmt::Return(e) => {
                if let Some(e) = e {
                    let v = self.ev(e);
                    self.exit(&v);
                }
            }
            Stmt::If(c, a, b) => {
                let cv = self.ev(c);
                self.exit(&cv);
                let before = self.env.clone();
                self.stmt(a);
                let then_env = std::mem::replace(&mut self.env, before.clone());
                if let Some(b) = b {
                    self.stmt(b);
                }
                let else_env = std::mem::replace(&mut self.env, before);
                self.merge(then_env, else_env);
            }
            Stmt::For(init, cond, step, body) => {
                self.env.push(HashMap::new());
                if let Some(i) = init {
                    self.stmt(i);
                }
                let mut names = HashSet::new();
                scan_stmt(body, &mut names);
                for e in cond.iter().chain(step.iter()) {
                    scan_expr(e, &mut names);
                }
                let phis = self.loop_entry(&names);
                if let Some(c) = cond {
                    let v = self.ev(c);
                    self.exit(&v);
                }
                self.stmt(body);
                if let Some(st) = step {
                    self.ev(st);
                }
                self.loop_exit(phis);
                self.env.pop();
            }
            Stmt::While(c, body) => {
                let mut names = HashSet::new();
                scan_expr(c, &mut names);
                scan_stmt(body, &mut names);
                let phis = self.loop_entry(&names);
                let v = self.ev(c);
                self.exit(&v);
                self.stmt(body);
                self.loop_exit(phis);
            }
            Stmt::DoWhile(body, c) => {
                let mut names = HashSet::new();
                scan_expr(c, &mut names);
                scan_stmt(body, &mut names);
                let phis = self.loop_entry(&names);
                self.stmt(body);
                let v = self.ev(c);
                self.exit(&v);
                self.loop_exit(phis);
            }
        }
    }

    /// Variables written in a loop are phis at its entry.
    fn loop_entry(&mut self, names: &HashSet<String>) -> Vec<(String, usize, N)> {
        let mut phis = Vec::new();
        let mut sorted: Vec<&String> = names.iter().collect();
        sorted.sort();
        for name in sorted {
            let Some(var) = self.lookup(name).cloned() else {
                continue;
            };
            let cur = self.comps(&var.val);
            let mut next = Vec::with_capacity(cur.len());
            for (i, c) in cur.iter().enumerate() {
                let phi = self.other(vec![*c]);
                phis.push((name.clone(), i, phi));
                next.push(phi);
            }
            let v = match var.val {
                Val::V(_) => Val::V(next),
                Val::Agg(_) => Val::Agg(next[0]),
            };
            self.set_var(name, v);
        }
        phis
    }

    fn loop_exit(&mut self, phis: Vec<(String, usize, N)>) {
        for (name, i, phi) in phis {
            let Some(var) = self.lookup(&name).cloned() else {
                continue;
            };
            let c = self.comps(&var.val);
            let Some(&last) = c.get(i).or(c.first()) else {
                continue;
            };
            self.nodes[phi].srcs.push(last);
            self.nodes[last].uses.push(phi);
        }
    }

    /// Join of the environments after the two branches of an `if`.
    fn merge(&mut self, a: Env, b: Env) {
        for (si, (sa, sb)) in a.iter().zip(&b).enumerate() {
            for (name, va) in sa {
                let Some(vb) = sb.get(name) else { continue };
                let (ca, cb) = (self.comps(&va.val), self.comps(&vb.val));
                if ca == cb {
                    if let Some(slot) = self.env.get_mut(si).and_then(|s| s.get_mut(name)) {
                        slot.val = va.val.clone();
                    }
                    continue;
                }
                let merged: Vec<N> = ca
                    .iter()
                    .zip(&cb)
                    .map(|(&x, &y)| if x == y { x } else { self.other(vec![x, y]) })
                    .collect();
                let val = match va.val {
                    Val::V(_) => Val::V(merged),
                    Val::Agg(_) => Val::Agg(merged[0]),
                };
                if let Some(slot) = self.env.get_mut(si).and_then(|s| s.get_mut(name)) {
                    slot.val = val;
                }
            }
        }
    }

    // ---------- the pass ----------

    fn run(&mut self, ps: &Func) {
        self.env.push(HashMap::new());
        for p in &ps.params {
            if p.dir != Dir::In {
                continue; // an output parameter: assigning it is a store
            }
            let input = match p.name.as_str() {
                "_uv" => Some(Input::Uv),
                "_rad_ang" => Some(Input::RadAng),
                "_vDiffuse" => Some(Input::Diffuse),
                _ => None,
            };
            let val = match input {
                Some(i) => {
                    let d = p.ty.dim() as usize;
                    Val::V((0..d).map(|c| self.load(i, c)).collect())
                }
                None => {
                    let o = self.agg(Vec::new());
                    self.fit(o, &p.ty)
                }
            };
            if let Some(sc) = self.env.last_mut() {
                sc.insert(
                    p.name.clone(),
                    Var {
                        ty: p.ty.clone(),
                        val,
                    },
                );
            }
        }
        self.env.push(HashMap::new());
        for s in &ps.body {
            self.stmt(s);
        }
        self.env.pop();
    }

    /// Dead code elimination: live = reaches a store, an `if` condition or a discard.
    fn mark_live(&mut self) {
        let mut stack: Vec<N> = (0..self.nodes.len())
            .filter(|&n| self.nodes[n].exit)
            .collect();
        while let Some(n) = stack.pop() {
            if self.nodes[n].live {
                continue;
            }
            self.nodes[n].live = true;
            stack.extend(self.nodes[n].srcs.iter().copied());
        }
    }

    /// Mesa's loop: returns the movable post-dominators, in the order they are moved.
    fn motion(&mut self) -> Vec<Moved> {
        let mut roots: Vec<Moved> = Vec::new();
        loop {
            let len = self.nodes.len();
            let mut ipdom = vec![ROOT; len];
            let mut depth = vec![0usize; len];
            for n in (0..len).rev() {
                let node = &self.nodes[n];
                if !node.live || node.class != Class::Interp {
                    continue;
                }
                let uses: Vec<N> = node
                    .uses
                    .iter()
                    .copied()
                    .filter(|&u| self.nodes[u].live)
                    .collect();
                // a store, an `if` condition or an unmovable use ends the use paths at the dummy root; otherwise the
                // paths of all (interpolated) uses meet at their lowest common post-dominator
                let idom = if node.exit
                    || uses.is_empty()
                    || uses.iter().any(|&u| self.nodes[u].class != Class::Interp)
                {
                    ROOT
                } else {
                    uses[1..]
                        .iter()
                        .fold(uses[0], |a, &b| lca(&ipdom, &depth, a, b))
                };
                ipdom[n] = idom;
                depth[n] = if idom == ROOT { 1 } else { depth[idom] + 1 };
            }
            // start loads in creation order: the farthest movable post-dominator of a load is moved
            let mut next = None;
            for l in 0..len {
                if !matches!(self.nodes[l].kind, Kind::Load(..) | Kind::Folded)
                    || !self.nodes[l].live
                {
                    continue;
                }
                let mut iter = l;
                let mut far = None;
                while ipdom[iter] != ROOT {
                    iter = ipdom[iter];
                    far = Some(iter);
                }
                if far.is_some() {
                    next = far;
                    break;
                }
            }
            let Some(p) = next else { break };
            // the post-dominator becomes a new input: its sources lose this use
            let srcs = match &self.nodes[p].kind {
                Kind::Add(a, b) | Kind::Sub(a, b) | Kind::Mul(a, b) | Kind::Div(a, b) => {
                    vec![*a, *b]
                }
                Kind::Lerp(a, b, t) => vec![*a, *b, *t],
                _ => Vec::new(),
            };
            for s in srcs {
                self.nodes[s].uses.retain(|&u| u != p);
            }
            self.nodes[p].srcs.clear();
            let kind = std::mem::replace(&mut self.nodes[p].kind, Kind::Folded);
            roots.push(Moved { node: p, kind });
            // loads that lost every live use are dead now
            for n in self.nodes.iter_mut() {
                n.live = false;
            }
            self.mark_live();
        }
        roots
    }
}

/// The dummy root of the use graph: stores, `if` conditions and unmovable instructions.
const ROOT: usize = usize::MAX;

/// Lowest common post-dominator of `a` and `b`.
fn lca(ipdom: &[usize], depth: &[usize], mut a: usize, mut b: usize) -> usize {
    while a != b {
        if a == ROOT || b == ROOT {
            return ROOT;
        }
        match depth[a].cmp(&depth[b]) {
            std::cmp::Ordering::Greater => a = ipdom[a],
            std::cmp::Ordering::Less => b = ipdom[b],
            std::cmp::Ordering::Equal => {
                a = ipdom[a];
                b = ipdom[b];
            }
        }
    }
    a
}

/// A post-dominator that became an input: the instruction it was.
struct Moved {
    node: N,
    kind: Kind,
}

/// Per-component functions (a vector argument is processed per component, the result has the widest argument's width).
fn is_componentwise(name: &str) -> bool {
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
            | "step"
            | "min"
            | "max"
            | "smoothstep"
            | "clamp"
    )
}

fn lvalue_root_expr(e: &Expr) -> Option<&str> {
    match e {
        Expr::Ident(n) => Some(n),
        Expr::Member(b, _) | Expr::Index(b, _) => lvalue_root_expr(b),
        _ => None,
    }
}

/// The vertex-stage text of node `n`; below a fold root everything is a plain expression, an earlier fold is a call.
fn render(
    nodes: &[Node],
    moved: &[Moved],
    tr: &mut Tr,
    n: N,
    top: bool,
    lines: &mut Vec<String>,
) -> Option<String> {
    let folded = moved.iter().position(|m| m.node == n);
    if !top && let Some(k) = folded {
        return Some(format!("fold_{k}(vs_uv, vs_rad_ang, vs_diffuse)"));
    }
    let kind = match folded {
        Some(k) => &moved[k].kind,
        None => &nodes[n].kind,
    };
    let bin = |tr: &mut Tr, op: &str, a: N, b: N, lines: &mut Vec<String>| {
        let x = render(nodes, moved, tr, a, false, lines)?;
        let y = render(nodes, moved, tr, b, false, lines)?;
        Some(format!("({x} {op} {y})"))
    };
    Some(match kind {
        Kind::Conv => {
            let ast = nodes[n].ast.as_ref()?;
            let saved = std::mem::take(&mut tr.cx);
            tr.cx.scopes.push(HashMap::new());
            let r = tr.expr(ast);
            let got = std::mem::take(&mut tr.cx.lines);
            tr.cx = saved;
            let v = r.ok()?;
            lines.extend(got);
            v.s
        }
        Kind::Load(input, c) => {
            let sw = ["x", "y", "z", "w"][*c];
            match input {
                Input::Uv => format!("vs_uv.{sw}"),
                Input::RadAng => format!("vs_rad_ang.{sw}"),
                Input::Diffuse => format!("vs_diffuse.{sw}"),
            }
        }
        Kind::Add(a, b) => bin(tr, "+", *a, *b, lines)?,
        Kind::Sub(a, b) => bin(tr, "-", *a, *b, lines)?,
        Kind::Mul(a, b) => bin(tr, "*", *a, *b, lines)?,
        Kind::Div(a, b) => bin(tr, "/", *a, *b, lines)?,
        Kind::Lerp(a, b, t) => {
            let x = render(nodes, moved, tr, *a, false, lines)?;
            let y = render(nodes, moved, tr, *b, false, lines)?;
            let w = render(nodes, moved, tr, *t, false, lines)?;
            format!("mix({x}, {y}, {w})")
        }
        Kind::Neg(_) | Kind::Folded | Kind::Other => return None,
    })
}

impl<'a> Tr<'a> {
    /// Decide which values of `PS` Mesa folds into the vertex stage (see the module docs) and prepare their vertex
    /// functions and the sites that read them. Leaves the plan empty when anything cannot be rendered.
    pub(crate) fn plan_folds(&mut self, kind: PassKind) {
        let prog: &'a Program = self.prog;
        let Some(ps) = self
            .fns
            .get("PS")
            .and_then(|v| v.first())
            .map(|u| &prog.funcs[u.idx])
        else {
            return;
        };
        let (nodes, moved) = {
            let mut an = An {
                tr: &*self,
                kind,
                nodes: Vec::new(),
                intern: HashMap::new(),
                env: Vec::new(),
            };
            an.run(ps);
            an.mark_live();
            let moved = an.motion();
            (an.nodes, moved)
        };
        if moved.is_empty() || moved.len() > MAX_FOLDS {
            return;
        }
        let mut folds = Vec::new();
        for (k, m) in moved.iter().enumerate() {
            let mut lines = Vec::new();
            let Some(expr) = render(&nodes, &moved, self, m.node, true, &mut lines) else {
                return;
            };
            let mut f = format!(
                "fn fold_{k}(vs_uv: vec4<f32>, vs_rad_ang: vec2<f32>, vs_diffuse: vec4<f32>) -> f32 {{\n"
            );
            for l in lines {
                f.push_str(&l);
                f.push('\n');
            }
            f.push_str(&format!("    return {expr};\n}}\n"));
            folds.push(f);
        }
        let mut roots: HashMap<usize, Vec<Option<usize>>> = HashMap::new();
        for (k, m) in moved.iter().enumerate() {
            for &(site, comp) in &nodes[m.node].sites {
                let spec = roots.entry(site).or_default();
                if spec.len() <= comp {
                    spec.resize(comp + 1, None);
                }
                spec[comp] = Some(k);
            }
        }
        for j in 0..folds.len().div_ceil(4) {
            self.private_decls
                .push(format!("var<private> g_fold{j}: vec4<f32>;"));
        }
        self.fold_roots = roots;
        self.folds = folds;
    }
}
