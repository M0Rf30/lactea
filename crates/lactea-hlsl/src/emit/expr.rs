//! Expression translation.

use super::*;

const SW: [char; 4] = ['x', 'y', 'z', 'w'];

pub(crate) enum LvKind {
    Plain(String),
    /// base vector lvalue + component indices (write mask / reorder)
    Swz(String, Vec<usize>),
}

pub(crate) struct Lv {
    pub kind: LvKind,
    pub ty: Ty,
}

pub(crate) fn parse_swizzle(m: &str) -> Option<Vec<usize>> {
    if m.is_empty() || m.len() > 4 {
        return None;
    }
    let xyzw = m.chars().all(|c| "xyzw".contains(c));
    let rgba = m.chars().all(|c| "rgba".contains(c));
    if !xyzw && !rgba {
        return None;
    }
    Some(
        m.chars()
            .map(|c| match c {
                'x' | 'r' => 0,
                'y' | 'g' => 1,
                'z' | 'b' => 2,
                _ => 3,
            })
            .collect(),
    )
}

impl Tr<'_> {
    pub fn has_se(&self, e: &Expr) -> bool {
        match e {
            Expr::Float(_) | Expr::Int(_) | Expr::Bool(_) | Expr::Ident(_) => false,
            Expr::Assign(..) | Expr::IncDec { .. } | Expr::Comma(_) => true,
            Expr::Unary(_, a) | Expr::Cast(_, a) | Expr::Member(a, _) => self.has_se(a),
            Expr::Binary(_, a, b) | Expr::Index(a, b) => self.has_se(a) || self.has_se(b),
            Expr::Cond(a, b, c) => self.has_se(a) || self.has_se(b) || self.has_se(c),
            Expr::Ctor(_, v) | Expr::InitList(v) => v.iter().any(|x| self.has_se(x)),
            Expr::Call(n, v) => {
                if matches!(n.as_str(), "sincos" | "modf" | "clip") {
                    return true;
                }
                if let Some(c) = self.fns.get(n)
                    && c.iter().any(|u| {
                        self.prog.funcs[u.idx]
                            .params
                            .iter()
                            .any(|p| p.dir != Dir::In)
                    })
                {
                    return true;
                }
                v.iter().any(|x| self.has_se(x))
            }
        }
    }

    /// Evaluate operands left to right; earlier operands are frozen into temps when a
    /// later operand has side effects.
    pub fn eval_args(&mut self, es: &[&Expr]) -> R<Vec<V>> {
        let mut vs: Vec<V> = Vec::new();
        for (i, e) in es.iter().enumerate() {
            if i > 0 && self.has_se(e) {
                for slot in vs.iter_mut().take(i) {
                    let v = slot.clone();
                    *slot = self.freeze(v);
                }
            }
            vs.push(self.expr(e)?);
        }
        Ok(vs)
    }

    pub fn expr(&mut self, e: &Expr) -> R<V> {
        match e {
            Expr::Float(v) => {
                let v = if self.pm_flavor {
                    round_literal_6(*v)
                } else {
                    *v
                };
                Ok(V::new(fmt_f32(v), Ty::F))
            }
            Expr::Int(v) => Ok(V::new(fmt_f32(*v as f64), Ty::I)),
            Expr::Bool(b) => Ok(V::new(if *b { "true" } else { "false" }, Ty::B)),
            Expr::Ident(n) => {
                if let Some(l) = self.lookup_local(n) {
                    let (w, ty, ptr) = (l.w.clone(), l.ty.clone(), l.ptr);
                    return Ok(V::new(if ptr { format!("(*{w})") } else { w }, ty));
                }
                if let Some(v) = self.global_value(n)? {
                    return Ok(v);
                }
                terr(format!("undefined identifier `{n}`"))
            }
            Expr::Unary(op, a) => {
                let v = self.expr(a)?;
                self.unary(*op, v)
            }
            Expr::Binary(op, a, b) => {
                if matches!(op, BinOp::And | BinOp::Or) && self.has_se(b) {
                    return self.short_circuit(*op, a, b);
                }
                let vs = self.eval_args(&[a, b])?;
                self.binary(*op, vs[0].clone(), vs[1].clone())
            }
            Expr::Assign(op, l, r) => self.assign(*op, l, r),
            Expr::IncDec { inc, prefix, e } => {
                let lv = self.lvalue(e)?;
                let cur = self.lv_read(&lv);
                let old = if *prefix {
                    None
                } else {
                    Some(self.freeze(cur.clone()))
                };
                let one = V::new("1.0f", Ty::I);
                let nv = self.binary(if *inc { BinOp::Add } else { BinOp::Sub }, cur, one)?;
                let nv = self.conv_v(&nv, &lv.ty)?;
                self.store(&lv, &nv.s)?;
                Ok(match old {
                    Some(o) => o,
                    None => self.lv_read(&lv),
                })
            }
            Expr::Cond(c, a, b) => self.cond(c, a, b),
            Expr::Comma(v) => {
                let mut last = None;
                for (i, x) in v.iter().enumerate() {
                    if i + 1 < v.len() {
                        self.expr_stmt(x)?;
                    } else {
                        last = Some(self.expr(x)?);
                    }
                }
                last.ok_or_else(|| TranslateError::Parse("empty comma expression".into()))
            }
            Expr::Call(n, args) => self.call(n, args),
            Expr::Ctor(ty, args) => {
                let refs: Vec<&Expr> = args.iter().collect();
                let vs = self.eval_args(&refs)?;
                self.ctor(ty, vs)
            }
            Expr::Cast(ty, a) => {
                let v = self.expr(a)?;
                match ty {
                    Ty::Void => terr("cast to void"),
                    _ => {
                        let s = self.conv(&v, ty)?;
                        Ok(V::new(s, ty.clone()))
                    }
                }
            }
            Expr::Index(b, i) => {
                // matrix element m[i][j]  (HLSL row i, column j) -> WGSL m[j][i]
                if let Expr::Index(mb, ri) = &**b {
                    let mv = self.expr(mb)?;
                    if let Ty::Mat(..) = mv.ty {
                        let r = self.index_str(ri)?;
                        let c = self.index_str(i)?;
                        return Ok(V::new(format!("{}[{c}][{r}]", mv.s), Ty::F));
                    }
                }
                let bv = self.expr(b)?;
                let idx = self.index_str(i)?;
                match &bv.ty {
                    Ty::Arr(el, _) => Ok(V::new(format!("{}[{idx}]", bv.s), (**el).clone())),
                    Ty::Vec(s, _) => Ok(V::new(format!("{}[{idx}]", mp(&bv.s)), Ty::Sc(*s))),
                    Ty::Mat(_, c) => Ok(V::new(
                        format!("transpose({})[{idx}]", bv.s),
                        Ty::vec(Sc::F, *c),
                    )),
                    t => terr(format!("cannot index {}", t.name())),
                }
            }
            Expr::Member(b, m) => {
                let bv = self.expr(b)?;
                self.member(bv, m)
            }
            Expr::InitList(_) => terr("initializer list in expression context"),
        }
    }

    fn index_str(&mut self, i: &Expr) -> R<String> {
        if let Expr::Int(v) = i
            && *v >= 0
        {
            return Ok(v.to_string());
        }
        let v = self.expr(i)?;
        let s = self.conv(&v, &Ty::F).or_else(|_| self.conv(&v, &Ty::I))?;
        Ok(format!("i32({s})"))
    }

    pub fn member(&mut self, bv: V, m: &str) -> R<V> {
        let idx = match parse_swizzle(m) {
            Some(i) => i,
            None => return terr(format!("unsupported member `.{m}` on {}", bv.ty.name())),
        };
        match &bv.ty {
            Ty::Sc(s) => {
                if idx.iter().any(|&i| i != 0) {
                    return terr(format!("bad swizzle `.{m}` on scalar"));
                }
                if idx.len() == 1 {
                    Ok(bv)
                } else {
                    let ty = Ty::vec(*s, idx.len() as u8);
                    Ok(V::new(format!("{}({})", wgsl_ty(&ty), bv.s), ty))
                }
            }
            Ty::Vec(s, n) => {
                if idx.iter().any(|&i| i >= *n as usize) {
                    return terr(format!("swizzle `.{m}` out of range for {}", bv.ty.name()));
                }
                let sw: String = idx.iter().map(|&i| SW[i]).collect();
                let ty = Ty::vec(*s, idx.len() as u8);
                Ok(V::new(format!("{}.{sw}", mp(&bv.s)), ty))
            }
            t => terr(format!("member access `.{m}` on {}", t.name())),
        }
    }

    fn short_circuit(&mut self, op: BinOp, a: &Expr, b: &Expr) -> R<V> {
        let av = self.expr(a)?;
        let ab = self.to_bool(&av)?;
        let t = self.fresh("sc");
        self.line(format!("var {t}: bool = {ab};"));
        let cond = if op == BinOp::And {
            t.clone()
        } else {
            format!("!{t}")
        };
        self.open(format!("if ({cond}) {{"));
        let bv = self.expr(b)?;
        let bb = self.to_bool(&bv)?;
        self.line(format!("{t} = {bb};"));
        self.close();
        Ok(V::new(t, Ty::B))
    }

    fn cond(&mut self, c: &Expr, a: &Expr, b: &Expr) -> R<V> {
        if self.has_se(a) || self.has_se(b) {
            let cv = self.expr(c)?;
            let cb = self.to_bool(&cv)?;
            let (la, va) = self.capture(|s| s.expr(a))?;
            let (lb, vb) = self.capture(|s| s.expr(b))?;
            let ty = self.common_or_same(&va.ty, &vb.ty)?;
            let t = self.fresh("cs");
            self.line(format!("var {t}: {};", wgsl_ty(&ty)));
            self.open(format!("if ({cb}) {{"));
            for l in la {
                self.cx.lines.push(format!("    {l}"));
            }
            let s = self.conv(&va, &ty)?;
            self.line(format!("{t} = {s};"));
            self.close_with("} else {");
            self.cx.indent += 1;
            for l in lb {
                self.cx.lines.push(format!("    {l}"));
            }
            let s = self.conv(&vb, &ty)?;
            self.line(format!("{t} = {s};"));
            self.close();
            return Ok(V::new(t, ty));
        }
        let vs = self.eval_args(&[c, a, b])?;
        let (cv, av, bv) = (&vs[0], &vs[1], &vs[2]);
        let ty = self.common_or_same(&av.ty, &bv.ty)?;
        let a_s = self.conv(av, &ty)?;
        let b_s = self.conv(bv, &ty)?;
        let cty = match &cv.ty {
            Ty::Vec(_, n) if ty.dim() > 1 => Ty::vec(Sc::B, (*n).min(ty.dim())),
            _ => Ty::B,
        };
        // vector condition: component-wise select, dims unified
        let (ty, a_s, b_s) = if cty.dim() > 1 && cty.dim() < ty.dim() {
            let t2 = Ty::vec(ty.sc().unwrap_or(Sc::F), cty.dim());
            let a2 = self.conv(&V::new(a_s, ty.clone()), &t2)?;
            let b2 = self.conv(&V::new(b_s, ty.clone()), &t2)?;
            (t2, a2, b2)
        } else {
            (ty, a_s, b_s)
        };
        let c_s = self.conv(cv, &cty)?;
        Ok(V::new(format!("select({b_s}, {a_s}, {c_s})"), ty))
    }

    fn common_or_same(&self, a: &Ty, b: &Ty) -> R<Ty> {
        if a == b {
            return Ok(a.clone());
        }
        self.common(a, b, true)
    }

    pub fn unary(&mut self, op: UnOp, v: V) -> R<V> {
        match op {
            UnOp::Plus => {
                if v.ty.sc() == Some(Sc::B) {
                    let ty = Ty::vec(Sc::I, v.ty.dim());
                    return self.conv_v(&v, &ty);
                }
                Ok(v)
            }
            UnOp::Neg => {
                let v = if v.ty.sc() == Some(Sc::B) {
                    let ty = Ty::vec(Sc::I, v.ty.dim());
                    self.conv_v(&v, &ty)?
                } else {
                    v
                };
                match v.ty {
                    Ty::Mat(..) => Ok(V::new(format!("({} * -1.0f)", paren(&v.s)), v.ty)),
                    Ty::Sc(_) | Ty::Vec(..) => Ok(V::new(format!("(-{})", paren(&v.s)), v.ty)),
                    _ => terr("unary minus on non-numeric"),
                }
            }
            UnOp::Not => {
                let bty = Ty::vec(Sc::B, v.ty.dim().max(1));
                let s = self.conv(&v, &bty)?;
                Ok(V::new(format!("(!{})", paren(&s)), bty))
            }
            UnOp::BitNot => Err(TranslateError::Unsupported(
                "bitwise not on ps_2/ps_3 (no integer ops)".into(),
            )),
        }
    }

    pub fn binary(&mut self, op: BinOp, a: V, b: V) -> R<V> {
        use BinOp::*;
        if matches!(op, BitAnd | BitOr | BitXor | Shl | Shr) {
            return Err(TranslateError::Unsupported(
                "bitwise operators (ps_2/ps_3 have no integer ops)".into(),
            ));
        }
        // matrices
        if matches!(a.ty, Ty::Mat(..)) || matches!(b.ty, Ty::Mat(..)) {
            return self.mat_binary(op, a, b);
        }
        match op {
            Add | Sub | Mul | Div | Rem => {
                let rt = self.common(&a.ty, &b.ty, false)?;
                let rsc = rt.sc().ok_or_else(|| {
                    TranslateError::Type("arithmetic on non-numeric operands".into())
                })?;
                let target = |t: &Ty| {
                    if t.dim() == 1 {
                        Ty::Sc(rsc)
                    } else {
                        Ty::vec(rsc, rt.dim())
                    }
                };
                let (ta, tb) = (target(&a.ty), target(&b.ty));
                let sa = self.conv(&a, &ta)?;
                let mut sb = self.conv(&b, &tb)?;
                let zero_div = matches!(op, Div | Rem)
                    && (sb == "0.0f" || (sb.starts_with("vec") && sb.ends_with("(0.0f)")));
                // projectM: the GL compiler folds `0 * x` first and then evaluates the literal `0 / 0` to NaN, while the
                // SPIR-V path of wgpu reduces it to 0 (`0 * rcp(0)` simplified before folding): force IEEE for a
                // divisor that is a literal with a zero component
                if op == Div && rt.sc() == Some(Sc::F) && self.pm_flavor && literal_has_zero(&sb) {
                    return Ok(self.div_ieee(sa, sb, &ta, &tb, rt));
                }
                if zero_div {
                    // WGSL const-evaluation rejects division by a literal zero; HLSL yields inf/NaN
                    let z = self.fresh("zero");
                    self.line(format!("var {z}: {} = {sb};", wgsl_ty(&tb)));
                    sb = z;
                }
                if op == Mul
                    && self.pm_flavor
                    && !self.plain_mul
                    && matches!(rt.sc(), Some(Sc::F | Sc::I))
                {
                    return Ok(self.mult_nan(sa, sb, &ta, &tb, rt));
                }
                let o = match op {
                    Add => "+",
                    Sub => "-",
                    Mul => "*",
                    Div => "/",
                    _ => "%",
                };
                if op == Div && rt.sc() == Some(Sc::I) {
                    // int / int: integer semantics (quotient truncated toward zero)
                    return Ok(V::new(format!("trunc({sa} / {sb})"), rt));
                }
                if op == Rem && self.floor_mod && rt.sc() == Some(Sc::F) {
                    return Ok(self.floor_mod_expr(sa, sb, rt));
                }
                Ok(V::new(format!("({sa} {o} {sb})"), rt))
            }
            Lt | Gt | Le | Ge | Eq | Ne => {
                let keep_bool =
                    matches!(op, Eq | Ne) && a.ty.sc() == Some(Sc::B) && b.ty.sc() == Some(Sc::B);
                let rt = self.common(&a.ty, &b.ty, keep_bool)?;
                let sa = self.conv(&a, &rt)?;
                let sb = self.conv(&b, &rt)?;
                let o = match op {
                    Lt => "<",
                    Gt => ">",
                    Le => "<=",
                    Ge => ">=",
                    Eq => "==",
                    _ => "!=",
                };
                Ok(V::new(format!("({sa} {o} {sb})"), Ty::vec(Sc::B, rt.dim())))
            }
            And | Or => {
                let d = if a.ty.dim() == 1 {
                    b.ty.dim()
                } else if b.ty.dim() == 1 {
                    a.ty.dim()
                } else {
                    a.ty.dim().min(b.ty.dim())
                };
                let bt = Ty::vec(Sc::B, d.max(1));
                let sa = self.conv(&a, &bt)?;
                let sb = self.conv(&b, &bt)?;
                let o = match (op, d) {
                    (And, 1) => "&&",
                    (Or, 1) => "||",
                    (And, _) => "&",
                    _ => "|",
                };
                Ok(V::new(format!("({sa} {o} {sb})"), bt))
            }
            _ => Err(TranslateError::Type(format!(
                "operator {op:?} is not valid on these operands"
            ))),
        }
    }

    /// projectM compiles with `Flag_AlternateNanPropagation`: every binary `*` is `mult(x, y)`,
    /// `x == 0 || y == 0 ? 0 : x * y` (D3D9's `0 * inf = 0`). As a side effect the product never feeds a fused
    /// multiply-add, unlike a plain `a * b + c` on the wgpu / SPIR-V path.
    fn mult_nan(&mut self, sa: String, sb: String, ta: &Ty, tb: &Ty, rt: Ty) -> V {
        let wt = wgsl_ty(&rt);
        let splat = |s: String, t: &Ty| {
            if t.dim() == 1 && rt.dim() > 1 {
                format!("{wt}({s})")
            } else {
                s
            }
        };
        let a = self.bind(V::new(splat(sa, ta), rt.clone())).s;
        let b = self.bind(V::new(splat(sb, tb), rt.clone())).s;
        let zero = if rt.dim() == 1 {
            "0.0f".to_string()
        } else {
            format!("{wt}(0.0f)")
        };
        V::new(
            format!("select(({a} * {b}), {zero}, (({a} == {zero}) | ({b} == {zero})))"),
            rt,
        )
    }

    /// `a / b` where `b` is a literal containing a zero component: `0 / 0` is NaN for certain (see `binary`).
    fn div_ieee(&mut self, sa: String, sb: String, ta: &Ty, tb: &Ty, rt: Ty) -> V {
        let wt = wgsl_ty(&rt);
        let splat = |s: String, t: &Ty| {
            if t.dim() == 1 && rt.dim() > 1 {
                format!("{wt}({s})")
            } else {
                s
            }
        };
        let a = self.bind(V::new(splat(sa, ta), rt.clone())).s;
        // a `var` keeps WGSL const-evaluation from rejecting the division by zero
        let b = self.fresh("zdiv");
        self.line(format!("var {b}: {wt} = {};", splat(sb, tb)));
        let u = self.fresh("nanbits");
        self.line(format!("var {u}: u32 = 0x7fc00000u;"));
        let (nan, zero) = if rt.dim() == 1 {
            (format!("bitcast<f32>({u})"), "0.0f".to_string())
        } else {
            (format!("{wt}(bitcast<f32>({u}))"), format!("{wt}(0.0f)"))
        };
        V::new(
            format!("select(({a} / {b}), {nan}, (({a} == {zero}) & ({b} == {zero})))"),
            rt,
        )
    }

    /// GLSL `mod(x, y)` = `x - y * floor(x / y)` for float operands (each evaluated once).
    pub fn floor_mod_expr(&mut self, a: String, b: String, ty: Ty) -> V {
        let a = self.bind(V::new(a, ty.clone()));
        let b = self.bind(V::new(b, ty.clone()));
        V::new(
            format!("({} - {} * floor({} / {}))", a.s, b.s, a.s, b.s),
            ty,
        )
    }

    fn mat_binary(&mut self, op: BinOp, a: V, b: V) -> R<V> {
        use BinOp::*;
        let (tya, tyb) = (a.ty.clone(), b.ty.clone());
        match (&tya, &tyb) {
            (Ty::Mat(r1, c1), Ty::Mat(r2, c2)) if r1 == r2 && c1 == c2 => {
                let (r, c) = (*r1, *c1);
                let ty = a.ty.clone();
                match op {
                    Add | Sub => Ok(V::new(
                        format!("({} {} {})", a.s, if op == Add { "+" } else { "-" }, b.s),
                        ty,
                    )),
                    Mul | Div => {
                        let (ma, mb) = (self.bind(a), self.bind(b));
                        let o = if op == Mul { "*" } else { "/" };
                        let cols: Vec<String> = (0..c)
                            .map(|j| format!("{}[{j}] {o} {}[{j}]", ma.s, mb.s))
                            .collect();
                        Ok(V::new(format!("mat{c}x{r}<f32>({})", cols.join(", ")), ty))
                    }
                    _ => terr("unsupported matrix operator"),
                }
            }
            (Ty::Mat(r, c), s) if s.is_numeric_like() && s.dim() == 1 => {
                let sv = self.conv_v(&b, &Ty::F)?;
                let ty = a.ty.clone();
                match op {
                    Mul => Ok(V::new(format!("({} * {})", a.s, sv.s), ty)),
                    Div | Add | Sub => {
                        let (ma, ms) = (self.bind(a), self.bind(sv));
                        let o = match op {
                            Div => "/",
                            Add => "+",
                            _ => "-",
                        };
                        let cols: Vec<String> = (0..*c)
                            .map(|j| format!("{}[{j}] {o} vec{r}<f32>({})", ma.s, ms.s))
                            .collect();
                        Ok(V::new(format!("mat{c}x{r}<f32>({})", cols.join(", ")), ty))
                    }
                    _ => terr("unsupported matrix operator"),
                }
            }
            (s, Ty::Mat(r, c)) if s.is_numeric_like() && s.dim() == 1 => {
                let sv = self.conv_v(&a, &Ty::F)?;
                let ty = b.ty.clone();
                match op {
                    Mul => Ok(V::new(format!("({} * {})", sv.s, b.s), ty)),
                    Add | Sub => {
                        let (mb, ms) = (self.bind(b), self.bind(sv));
                        let o = if op == Add { "+" } else { "-" };
                        let cols: Vec<String> = (0..*c)
                            .map(|j| format!("vec{r}<f32>({}) {o} {}[{j}]", ms.s, mb.s))
                            .collect();
                        Ok(V::new(format!("mat{c}x{r}<f32>({})", cols.join(", ")), ty))
                    }
                    _ => terr("unsupported scalar/matrix operator"),
                }
            }
            _ => terr(format!(
                "unsupported matrix operands {} / {}",
                a.ty.name(),
                b.ty.name()
            )),
        }
    }

    // ---------- lvalues & assignment ----------
    pub fn lvalue(&mut self, e: &Expr) -> R<Lv> {
        match e {
            Expr::Ident(n) => {
                if let Some(l) = self.lookup_local(n) {
                    let (w, ty, ptr) = (l.w.clone(), l.ty.clone(), l.ptr);
                    return Ok(Lv {
                        kind: LvKind::Plain(if ptr { format!("(*{w})") } else { w }),
                        ty,
                    });
                }
                match self.global_value(n)? {
                    Some(v)
                        if self
                            .globals
                            .get(n)
                            .map(|g| matches!(g.kind, GKind::Private))
                            .unwrap_or(false) =>
                    {
                        Ok(Lv {
                            kind: LvKind::Plain(v.s),
                            ty: v.ty,
                        })
                    }
                    Some(_) => Err(TranslateError::Unsupported(format!(
                        "assignment to uniform `{n}`"
                    ))),
                    None => terr(format!("undefined identifier `{n}`")),
                }
            }
            Expr::Member(b, m) => {
                let base = self.lvalue(b)?;
                let idx = match parse_swizzle(m) {
                    Some(i) => i,
                    None => return terr(format!("bad swizzle `.{m}` in assignment")),
                };
                let sc = base.ty.sc().ok_or_else(|| {
                    TranslateError::Type("swizzle assignment on non-vector".into())
                })?;
                match (&base.ty, base.kind) {
                    (Ty::Sc(_), LvKind::Plain(s)) => {
                        if idx.len() != 1 || idx[0] != 0 {
                            return terr("bad swizzle on scalar lvalue");
                        }
                        Ok(Lv {
                            kind: LvKind::Plain(s),
                            ty: Ty::Sc(sc),
                        })
                    }
                    (Ty::Vec(_, n), LvKind::Plain(s)) => {
                        if idx.iter().any(|&i| i >= *n as usize) {
                            return terr("swizzle out of range");
                        }
                        if idx.len() == 1 {
                            Ok(Lv {
                                kind: LvKind::Plain(format!("{s}.{}", SW[idx[0]])),
                                ty: Ty::Sc(sc),
                            })
                        } else {
                            Ok(Lv {
                                kind: LvKind::Swz(s, idx.clone()),
                                ty: Ty::vec(sc, idx.len() as u8),
                            })
                        }
                    }
                    (Ty::Vec(..), LvKind::Swz(s, outer)) => {
                        if idx.iter().any(|&i| i >= outer.len()) {
                            return terr("swizzle out of range");
                        }
                        let comp: Vec<usize> = idx.iter().map(|&i| outer[i]).collect();
                        if comp.len() == 1 {
                            Ok(Lv {
                                kind: LvKind::Plain(format!("{s}.{}", SW[comp[0]])),
                                ty: Ty::Sc(sc),
                            })
                        } else {
                            Ok(Lv {
                                kind: LvKind::Swz(s, comp.clone()),
                                ty: Ty::vec(sc, comp.len() as u8),
                            })
                        }
                    }
                    (Ty::Sc(_), LvKind::Swz(..)) => terr("swizzle of swizzle scalar"),
                    _ => terr("unsupported swizzle assignment target"),
                }
            }
            Expr::Index(b, i) => {
                if let Expr::Index(mb, ri) = &**b {
                    let mv = self.lvalue(mb);
                    if let Ok(Lv {
                        kind: LvKind::Plain(ms),
                        ty: Ty::Mat(..),
                    }) = mv
                    {
                        let r = self.index_str(ri)?;
                        let c = self.index_str(i)?;
                        return Ok(Lv {
                            kind: LvKind::Plain(format!("{ms}[{c}][{r}]")),
                            ty: Ty::F,
                        });
                    }
                }
                let base = self.lvalue(b)?;
                let idx = self.index_str(i)?;
                match (base.kind, &base.ty) {
                    (LvKind::Plain(s), Ty::Arr(el, _)) => Ok(Lv {
                        kind: LvKind::Plain(format!("{s}[{idx}]")),
                        ty: (**el).clone(),
                    }),
                    (LvKind::Plain(s), Ty::Vec(sc, _)) => Ok(Lv {
                        kind: LvKind::Plain(format!("{s}[{idx}]")),
                        ty: Ty::Sc(*sc),
                    }),
                    _ => Err(TranslateError::Unsupported(
                        "assignment to this indexed lvalue".into(),
                    )),
                }
            }
            _ => terr("expression is not assignable"),
        }
    }

    pub fn lv_read(&self, lv: &Lv) -> V {
        match &lv.kind {
            LvKind::Plain(s) => V::new(s.clone(), lv.ty.clone()),
            LvKind::Swz(b, idx) => {
                let sw: String = idx.iter().map(|&i| SW[i]).collect();
                V::new(format!("{}.{sw}", mp(b)), lv.ty.clone())
            }
        }
    }

    /// Emit `lv = value` (value already of type `lv.ty`).
    pub fn store(&mut self, lv: &Lv, value: &str) -> R<()> {
        match &lv.kind {
            LvKind::Plain(s) => {
                self.line(format!("{s} = {value};"));
            }
            LvKind::Swz(b, idx) => {
                let t = self.fresh("sw");
                self.line(format!("let {t} = {value};"));
                for (k, &i) in idx.iter().enumerate() {
                    self.line(format!("{b}.{} = {t}.{};", SW[i], SW[k]));
                }
            }
        }
        Ok(())
    }

    fn assign(&mut self, op: Option<BinOp>, l: &Expr, r: &Expr) -> R<V> {
        let lv = self.lvalue(l)?;
        let rv = self.expr(r)?;
        let val = match op {
            None => rv,
            Some(o) => {
                let cur = self.lv_read(&lv);
                // hlslparser emits `a *= b` as is: only the binary `*` goes through `mult()`
                self.plain_mul = true;
                let v = self.binary(o, cur, rv);
                self.plain_mul = false;
                v?
            }
        };
        let s = match (&lv.ty, &val.ty) {
            (Ty::Arr(..), _) => self.conv(&val, &lv.ty)?,
            _ => self.conv(&val, &lv.ty)?,
        };
        self.store(&lv, &s)?;
        Ok(self.lv_read(&lv))
    }

    /// Expression evaluated for its side effects only.
    pub fn expr_stmt(&mut self, e: &Expr) -> R<()> {
        match e {
            Expr::Assign(op, l, r) => {
                self.assign(*op, l, r)?;
            }
            Expr::IncDec { inc, e, .. } => {
                let lv = self.lvalue(e)?;
                let cur = self.lv_read(&lv);
                let nv = self.binary(
                    if *inc { BinOp::Add } else { BinOp::Sub },
                    cur,
                    V::new("1.0f", Ty::I),
                )?;
                let nv = self.conv_v(&nv, &lv.ty)?;
                self.store(&lv, &nv.s)?;
            }
            Expr::Comma(v) => {
                for x in v {
                    self.expr_stmt(x)?;
                }
            }
            Expr::Call(..) => {
                let v = self.expr(e)?;
                if v.ty != Ty::Void && !v.s.is_empty() && !is_simple(&v.s) {
                    self.line(format!("_ = {};", v.s));
                }
            }
            _ => {
                if self.has_se(e) {
                    let v = self.expr(e)?;
                    if v.ty != Ty::Void && !is_simple(&v.s) {
                        self.line(format!("_ = {};", v.s));
                    }
                }
            }
        }
        Ok(())
    }

    // ---------- user function calls ----------
    fn type_distance(from: &Ty, to: &Ty) -> Option<u32> {
        if from == to {
            return Some(0);
        }
        match (from, to) {
            (Ty::Sc(a), Ty::Sc(b)) => Some(if a == b {
                0
            } else if *a == Sc::I && *b == Sc::F {
                1
            } else {
                3
            }),
            (Ty::Sc(_), Ty::Vec(..)) => Some(4),
            (Ty::Vec(a, n), Ty::Vec(b, m)) if m <= n => Some(if a == b {
                1 + (n - m) as u32
            } else {
                3 + (n - m) as u32
            }),
            (Ty::Vec(..), Ty::Sc(_)) => Some(6),
            (Ty::Mat(a, b), Ty::Mat(c, d)) if a == c && b == d => Some(0),
            _ => None,
        }
    }

    pub fn call(&mut self, name: &str, args: &[Expr]) -> R<V> {
        if self.fns.contains_key(name) {
            return self.call_user(name, args);
        }
        self.intrinsic(name, args)
    }

    fn call_user(&mut self, name: &str, args: &[Expr]) -> R<V> {
        let refs: Vec<&Expr> = args.iter().collect();
        let vs = self.eval_args(&refs)?;
        let cands: Vec<usize> = (0..self.fns[name].len())
            .filter(|&k| self.prog.funcs[self.fns[name][k].idx].params.len() == args.len())
            .collect();
        let mut best: Option<(u32, usize)> = None;
        for &k in &cands {
            let f = &self.prog.funcs[self.fns[name][k].idx];
            let mut score = 0;
            let mut ok = true;
            for (p, v) in f.params.iter().zip(&vs) {
                match Self::type_distance(&v.ty, &p.ty) {
                    Some(d) => score += d,
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok && best.map(|(s, _)| score < s).unwrap_or(true) {
                best = Some((score, k));
            }
        }
        let k = match best {
            Some((_, k)) => k,
            None => {
                if let Some(&k) = cands.first() {
                    k // report the precise conversion error below
                } else {
                    return terr(format!(
                        "no matching overload for call to `{name}` with {} arguments",
                        args.len()
                    ));
                }
            }
        };
        let fidx = self.fns[name][k].idx;
        let f: &Func = &self.prog.funcs[fidx];
        let wname = self.translate_fn(name, k)?;
        let mut call_args = Vec::new();
        let mut post: Vec<(Lv, String, Ty)> = Vec::new();
        for ((p, v), a) in f.params.iter().zip(&vs).zip(args) {
            match p.dir {
                Dir::In => call_args.push(self.conv(v, &p.ty)?),
                Dir::Out | Dir::InOut => {
                    let lv = self.lvalue(a)?;
                    let t = self.fresh("out");
                    let init = if p.dir == Dir::InOut {
                        self.conv(&self.lv_read(&lv), &p.ty)?
                    } else {
                        zero_value(&p.ty)
                    };
                    self.line(format!("var {t}: {} = {init};", wgsl_ty(&p.ty)));
                    call_args.push(format!("&{t}"));
                    post.push((lv, t, p.ty.clone()));
                }
            }
        }
        let call = format!("{wname}({})", call_args.join(", "));
        if f.ret == Ty::Void {
            self.line(format!("{call};"));
            for (lv, t, pty) in post {
                let s = self.conv(&V::new(t, pty), &lv.ty)?;
                self.store(&lv, &s)?;
            }
            return Ok(V::new("", Ty::Void));
        }
        if !post.is_empty() {
            let r = self.fresh("r");
            self.line(format!("let {r} = {call};"));
            for (lv, t, pty) in post {
                let s = self.conv(&V::new(t, pty), &lv.ty)?;
                self.store(&lv, &s)?;
            }
            return Ok(V::new(r, f.ret.clone()));
        }
        Ok(V::new(call, f.ret.clone()))
    }
}

pub(crate) fn paren(s: &str) -> String {
    if s.starts_with('(') && s.ends_with(')') || is_simple(s) {
        s.to_string()
    } else {
        format!("({s})")
    }
}

/// `s` is an emitted scalar literal `0.0f`, or a `vecN<f32>(..)` constructor of numeric literals with at least one zero.
fn literal_has_zero(s: &str) -> bool {
    let lit = |a: &str| -> Option<f32> {
        let mut a = a.trim();
        while a.starts_with('(') && a.ends_with(')') {
            a = a[1..a.len() - 1].trim();
        }
        a.strip_suffix('f').unwrap_or(a).parse::<f32>().ok()
    };
    if s.starts_with("vec") {
        let (Some(open), Some(close)) = (s.find('('), s.rfind(')')) else {
            return false;
        };
        let vals: Option<Vec<f32>> = s[open + 1..close].split(',').map(lit).collect();
        vals.is_some_and(|v| v.contains(&0.0))
    } else {
        lit(s) == Some(0.0)
    }
}
