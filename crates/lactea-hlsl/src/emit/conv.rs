//! HLSL implicit/explicit conversions, constructors and type unification.

use super::*;

const SW: [char; 4] = ['x', 'y', 'z', 'w'];

impl Tr<'_> {
    /// Convert the scalar kind of `s` (value of `n` components) without changing dimension.
    fn kind_conv(&self, s: &str, from: Sc, to: Sc, n: u8) -> String {
        if from == to || (from == Sc::I && to == Sc::F) || (from == Sc::F && to == Sc::I && false) {
            return s.to_string();
        }
        match (from, to) {
            (Sc::F, Sc::I) => format!("trunc({s})"),
            (Sc::B, Sc::F) | (Sc::B, Sc::I) => {
                if n == 1 {
                    format!("f32({s})")
                } else {
                    format!("vec{n}<f32>({s})")
                }
            }
            (_, Sc::B) => {
                if n == 1 {
                    format!("({s} != 0.0f)")
                } else {
                    format!("({s} != vec{n}<f32>(0.0f))")
                }
            }
            _ => s.to_string(),
        }
    }

    /// Convert `v` to type `to` following HLSL implicit conversion rules (also used for casts).
    pub fn conv(&mut self, v: &V, to: &Ty) -> R<String> {
        if &v.ty == to {
            return Ok(v.s.clone());
        }
        match (&v.ty, to) {
            (Ty::Sc(a), Ty::Sc(b)) => Ok(self.kind_conv(&v.s, *a, *b, 1)),
            (Ty::Sc(a), Ty::Vec(b, n)) => {
                let k = self.kind_conv(&v.s, *a, *b, 1);
                Ok(format!("{}({k})", wgsl_ty(to))
                    .replace(&format!("vec{n}<"), &format!("vec{n}<")))
            }
            (Ty::Vec(a, n), Ty::Sc(b)) => {
                let _ = n;
                let first = format!("{}.x", mp(&v.s));
                Ok(self.kind_conv(&first, *a, *b, 1))
            }
            (Ty::Vec(a, n), Ty::Vec(b, m)) => {
                if m > n {
                    return terr(format!(
                        "cannot convert {} to {} (would expand)",
                        v.ty.name(),
                        to.name()
                    ));
                }
                let base = if m < n {
                    format!("{}.{}", mp(&v.s), &"xyzw"[..*m as usize])
                } else {
                    v.s.clone()
                };
                Ok(self.kind_conv(&base, *a, *b, *m))
            }
            (Ty::Sc(a), Ty::Mat(r, c)) => {
                let k = self.kind_conv(&v.s, *a, Sc::F, 1);
                let vv = self.bind(V::new(k, Ty::F));
                let n = (*r as usize) * (*c as usize);
                let args = vec![vv.s.clone(); n].join(", ");
                Ok(format!("mat{c}x{r}<f32>({args})"))
            }
            (Ty::Mat(r1, c1), Ty::Mat(r2, c2)) if r1 == r2 && c1 == c2 => Ok(v.s.clone()),
            (Ty::Mat(r1, c1), Ty::Mat(r2, c2)) if r2 <= r1 && c2 <= c1 => {
                // truncating matrix cast: rebuild from elements
                let m = self.bind(v.clone());
                let mut cols = Vec::new();
                for j in 0..*c2 {
                    let mut comps = Vec::new();
                    for i in 0..*r2 {
                        comps.push(format!("{}[{j}][{i}]", m.s));
                    }
                    cols.push(comps.join(", "));
                }
                Ok(format!("mat{c2}x{r2}<f32>({})", cols.join(", ")))
            }
            (Ty::Arr(a, n), Ty::Arr(b, m)) if n == m && a == b => Ok(v.s.clone()),
            _ => terr(format!("cannot convert {} to {}", v.ty.name(), to.name())),
        }
    }

    pub fn conv_v(&mut self, v: &V, to: &Ty) -> R<V> {
        Ok(V::new(self.conv(v, to)?, to.clone()))
    }

    /// Condition value as a WGSL scalar bool.
    #[allow(clippy::wrong_self_convention)] // `to_*` here is "convert to WGSL bool", it emits code through `&mut self`
    pub fn to_bool(&mut self, v: &V) -> R<String> {
        self.conv(v, &Ty::B)
    }

    /// Common type for arithmetic/comparison/intrinsic operands.
    pub fn common(&self, a: &Ty, b: &Ty, keep_bool: bool) -> R<Ty> {
        if let (Ty::Mat(..), _) | (_, Ty::Mat(..)) = (a, b) {
            return terr(format!(
                "unsupported operand types {} / {}",
                a.name(),
                b.name()
            ));
        }
        let (sa, sb) = match (a.sc(), b.sc()) {
            (Some(x), Some(y)) => (x, y),
            _ => {
                return terr(format!(
                    "unsupported operand types {} / {}",
                    a.name(),
                    b.name()
                ));
            }
        };
        let rank = |s: Sc| match s {
            Sc::B => 0,
            Sc::I => 1,
            Sc::F => 2,
        };
        let mut sc = if rank(sa) >= rank(sb) { sa } else { sb };
        if sc == Sc::B && !keep_bool {
            sc = Sc::I;
        }
        let (da, db) = (a.dim(), b.dim());
        let d = if da == 1 {
            db
        } else if db == 1 {
            da
        } else {
            da.min(db)
        };
        Ok(Ty::vec(sc, d))
    }

    /// Build a WGSL expression for the components of a value as a flat list of scalars.
    fn components(&mut self, v: &V) -> R<Vec<String>> {
        match &v.ty {
            Ty::Sc(_) => Ok(vec![v.s.clone()]),
            Ty::Vec(_, n) => {
                let b = self.bind(v.clone());
                Ok((0..*n as usize)
                    .map(|i| format!("{}.{}", b.s, SW[i]))
                    .collect())
            }
            Ty::Mat(r, c) => {
                let b = self.bind(v.clone());
                let mut out = Vec::new();
                for i in 0..*r {
                    for j in 0..*c {
                        out.push(format!("{}[{j}][{i}]", b.s));
                    }
                }
                Ok(out)
            }
            t => terr(format!("cannot use {} as constructor argument", t.name())),
        }
    }

    /// Constructor call / cast target with already-evaluated arguments.
    pub fn ctor(&mut self, ty: &Ty, args: Vec<V>) -> R<V> {
        match ty {
            Ty::Sc(_) => {
                if args.len() != 1 {
                    return terr("scalar constructor needs one argument");
                }
                let s = self.conv(&args[0], ty)?;
                Ok(V::new(s, ty.clone()))
            }
            Ty::Vec(sc, n) => {
                if args.len() == 1 {
                    let a = &args[0];
                    return match &a.ty {
                        Ty::Sc(_) | Ty::Vec(..) => {
                            let s = self.conv(a, ty)?;
                            Ok(V::new(s, ty.clone()))
                        }
                        Ty::Mat(..) => {
                            let comps = self.components(a)?;
                            let k: Vec<String> = comps.into_iter().take(*n as usize).collect();
                            let s = format!("{}({})", wgsl_ty(ty), k.join(", "));
                            Ok(V::new(s, ty.clone()))
                        }
                        t => terr(format!("bad constructor argument {}", t.name())),
                    };
                }
                let mut parts = Vec::new();
                let mut total = 0;
                for a in &args {
                    let d = match &a.ty {
                        Ty::Sc(_) => 1,
                        Ty::Vec(_, n) => *n,
                        Ty::Mat(r, c) => r * c,
                        t => return terr(format!("bad constructor argument {}", t.name())),
                    };
                    if let Ty::Mat(..) = a.ty {
                        for c in self.components(a)? {
                            parts.push(self.kind_conv(&c, Sc::F, *sc, 1));
                        }
                    } else {
                        let from = a.ty.sc().unwrap_or(Sc::F);
                        parts.push(self.kind_conv(&a.s, from, *sc, d));
                    }
                    total += d;
                }
                if total < *n {
                    return terr(format!("not enough components for {}", ty.name()));
                }
                if total > *n {
                    // WGSL requires an exact count; HLSL takes the leading components.
                    let mut flat = Vec::new();
                    for (a, p) in args.iter().zip(parts) {
                        let tmp = V::new(p, Ty::vec(*sc, a.ty.dim().max(1)));
                        flat.extend(self.components(&tmp)?);
                    }
                    flat.truncate(*n as usize);
                    return Ok(V::new(
                        format!("{}({})", wgsl_ty(ty), flat.join(", ")),
                        ty.clone(),
                    ));
                }
                Ok(V::new(
                    format!("{}({})", wgsl_ty(ty), parts.join(", ")),
                    ty.clone(),
                ))
            }
            Ty::Mat(r, c) => {
                if args.len() == 1 && !matches!(args[0].ty, Ty::Vec(..)) {
                    let a = &args[0];
                    return match &a.ty {
                        Ty::Mat(..) | Ty::Sc(_) => {
                            let s = self.conv(a, ty)?;
                            Ok(V::new(s, ty.clone()))
                        }
                        t => terr(format!("bad matrix constructor argument {}", t.name())),
                    };
                }
                // rows as vectors
                if args.len() == *r as usize
                    && args
                        .iter()
                        .all(|a| a.ty.dim() == *c && matches!(a.ty, Ty::Vec(..)))
                {
                    let rows: Vec<String> = args
                        .iter()
                        .map(|a| self.kind_conv(&a.s, a.ty.sc().unwrap_or(Sc::F), Sc::F, *c))
                        .collect();
                    return Ok(V::new(
                        format!("transpose(mat{r}x{c}<f32>({}))", rows.join(", ")),
                        ty.clone(),
                    ));
                }
                let mut flat = Vec::new();
                for a in &args {
                    let from = a.ty.sc().unwrap_or(Sc::F);
                    for comp in self.components(a)? {
                        flat.push(self.kind_conv(&comp, from, Sc::F, 1));
                    }
                }
                if flat.len() != (*r as usize) * (*c as usize) {
                    return terr(format!(
                        "wrong number of components for {} ({})",
                        ty.name(),
                        flat.len()
                    ));
                }
                Ok(V::new(
                    format!("transpose(mat{r}x{c}<f32>({}))", flat.join(", ")),
                    ty.clone(),
                ))
            }
            Ty::Arr(..) => terr("array constructors are not supported"),
            t => terr(format!("cannot construct {}", t.name())),
        }
    }

    /// Value of an initializer (expression or `{...}` list) converted to `ty`.
    pub fn init_value(&mut self, ty: &Ty, init: &Expr) -> R<String> {
        if let Expr::InitList(items) = init {
            match ty {
                Ty::Arr(el, n) => {
                    let mut parts = Vec::new();
                    for it in items {
                        parts.push(self.init_value(el, it)?);
                    }
                    // HLSL pads missing elements with zero
                    while parts.len() < *n {
                        parts.push(zero_value(el));
                    }
                    parts.truncate(*n);
                    return Ok(format!("{}({})", wgsl_ty(ty), parts.join(", ")));
                }
                _ => {
                    let mut vs = Vec::new();
                    for it in items {
                        vs.push(self.expr(it)?);
                    }
                    return Ok(self.ctor(ty, vs)?.s);
                }
            }
        }
        let v = self.expr(init)?;
        self.conv(&v, ty)
    }
}
