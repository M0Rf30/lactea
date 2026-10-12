//! HLSL intrinsic functions -> WGSL.

use super::expr::paren;
use super::*;

impl Tr<'_> {
    fn args_v(&mut self, args: &[Expr]) -> R<Vec<V>> {
        let refs: Vec<&Expr> = args.iter().collect();
        self.eval_args(&refs)
    }

    /// Convert all operands to one common type (scalar splat / vector truncation / kind promotion).
    fn unify_all(&mut self, vs: &[V], force_float: bool) -> R<(Vec<String>, Ty)> {
        let mut ty = vs[0].ty.clone();
        for v in &vs[1..] {
            ty = self.common(&ty, &v.ty, false)?;
        }
        if vs.len() == 1 {
            ty = self.common(&ty, &ty, false)?;
        }
        if force_float {
            ty = Ty::vec(Sc::F, ty.dim());
        }
        let mut out = Vec::new();
        for v in vs {
            out.push(self.conv(v, &ty)?);
        }
        Ok((out, ty))
    }

    fn need(&self, name: &str, args: &[Expr], n: usize) -> R<()> {
        if args.len() != n {
            return terr(format!(
                "`{name}` expects {n} arguments, got {}",
                args.len()
            ));
        }
        Ok(())
    }

    fn sampler_of(&mut self, e: &Expr, is3d: bool) -> R<String> {
        let v = self.expr(e)?;
        match v.ty {
            Ty::Sampler(_) => {
                if is3d {
                    self.used_samplers.insert(v.s.clone(), true);
                }
                Ok(v.s)
            }
            _ => terr("first argument of a texture lookup must be a sampler"),
        }
    }

    pub fn intrinsic(&mut self, name: &str, args: &[Expr]) -> R<V> {
        // texture lookups
        let tex = match name {
            "tex2D" | "tex2d" => Some((false, "")),
            "tex2Dlod" => Some((false, "lod")),
            "tex2Dbias" => Some((false, "bias")),
            "tex2Dproj" => Some((false, "proj")),
            "tex2Dgrad" => Some((false, "grad")),
            "tex3D" | "tex3d" => Some((true, "")),
            "tex3Dlod" => Some((true, "lod")),
            "tex3Dbias" => Some((true, "bias")),
            "tex3Dproj" => Some((true, "proj")),
            "tex3Dgrad" => Some((true, "grad")),
            _ => None,
        };
        if let Some((is3d, kind)) = tex {
            return self.tex_lookup(name, args, is3d, kind);
        }
        match name {
            // ---- component-wise unary ----
            "abs" | "sign" => {
                self.need(name, args, 1)?;
                let v = self.expr(&args[0])?;
                let keep_int = v.ty.sc() == Some(Sc::I);
                let ty = Ty::vec(if keep_int { Sc::I } else { Sc::F }, v.ty.dim());
                let s = self.conv(&v, &ty)?;
                Ok(V::new(format!("{name}({s})"), ty))
            }
            "acos" | "asin" | "atan" | "ceil" | "cos" | "cosh" | "exp" | "exp2" | "floor"
            | "log" | "log2" | "sin" | "sinh" | "sqrt" | "tan" | "tanh" | "trunc" | "degrees"
            | "radians" | "saturate" | "frac" | "rsqrt" | "ddx" | "ddy" | "fwidth" | "round"
            | "log10" | "ddx_coarse" | "ddy_coarse" | "ddx_fine" | "ddy_fine" => {
                self.need(name, args, 1)?;
                let v = self.expr(&args[0])?;
                let ty = Ty::vec(Sc::F, v.ty.dim());
                if !v.ty.is_numeric_like() {
                    return terr(format!("`{name}` on {}", v.ty.name()));
                }
                let s = self.conv(&v, &ty)?;
                let out = match name {
                    "frac" => format!("fract({s})"),
                    // asin/acos beyond [-1,1]: projectM's GLSL generator (`GLSLGenerator.cpp`, `m_asinFunction` /
                    // `m_acosFunction`) substitutes a polynomial that "qualitatively matches DX9"; inside it is the plain function.
                    "asin" | "acos" if self.pm_flavor => {
                        let t = self.bind(V::new(s, ty.clone())).s;
                        let one = format!("{}(1.0f)", wgsl_ty(&ty));
                        let k = "0.39269908169872415f";
                        let outside = if name == "asin" {
                            format!("(({k} * sign({t})) * (abs({t}) - 3.0f) * (abs({t}) - 3.0f))")
                        } else {
                            format!(
                                "({k} * (4.0f - (sign({t}) * (abs({t}) - 3.0f)) * (abs({t}) - 3.0f)))"
                            )
                        };
                        format!("select({name}({t}), {outside}, (abs({t}) > {one}))")
                    }
                    // projectM's GLSL generator (hlslparser) wraps these in abs(): D3D9 sm3 takes |x| for
                    // rsqrt/sqrt/log/log2/log10 instead of returning NaN (projectM issue #724).
                    "rsqrt" => format!("inverseSqrt(abs({s}))"),
                    "sqrt" | "log" | "log2" => format!("{name}(abs({s}))"),
                    "log10" => format!("(log(abs({s})) / log(10.0f))"),
                    "ddx" | "ddx_coarse" => format!("dpdx({s})"),
                    "ddy" | "ddy_coarse" => format!("dpdy({s})"),
                    "ddx_fine" => format!("dpdxFine({s})"),
                    "ddy_fine" => format!("dpdyFine({s})"),
                    // D3D9 round() is floor(x + 0.5) (half rounds up), unlike WGSL's round-half-even
                    "round" => format!("floor({s} + {}(0.5f))", wgsl_ty(&ty)),
                    _ => format!("{name}({s})"),
                };
                Ok(V::new(out, ty))
            }
            "isnan" | "isinf" | "isfinite" => {
                self.need(name, args, 1)?;
                let v = self.expr(&args[0])?;
                let ty = Ty::vec(Sc::F, v.ty.dim());
                let s = self.conv(&v, &ty)?;
                let t = self.bind(V::new(s, ty.clone()));
                let m = format!("{}(3.4028235e38f)", wgsl_ty(&ty));
                let out = match name {
                    "isnan" => format!("({} != {})", t.s, t.s),
                    "isinf" => format!("(abs({}) > {m})", t.s),
                    _ => format!("(abs({}) <= {m})", t.s),
                };
                Ok(V::new(out, Ty::vec(Sc::B, ty.dim())))
            }
            // ---- binary / ternary component-wise ----
            "atan2" | "pow" | "fmod" | "step" | "min" | "max" => {
                self.need(name, args, 2)?;
                let vs = self.args_v(args)?;
                let keep_int =
                    matches!(name, "min" | "max") && vs.iter().all(|v| v.ty.sc() == Some(Sc::I));
                let (s, mut ty) = self.unify_all(&vs, !keep_int)?;
                if keep_int {
                    ty = Ty::vec(Sc::I, ty.dim());
                }
                let out = match name {
                    "fmod" if self.floor_mod && ty.sc() == Some(Sc::F) => {
                        return Ok(self.floor_mod_expr(s[0].clone(), s[1].clone(), ty));
                    }
                    "fmod" => format!("({} % {})", s[0], s[1]),
                    // hlslparser: `pow(abs(x), y)`, except a literal exponent of 1 which returns x unchanged
                    // (also for negative x, where an unoptimised pow would be NaN).
                    "pow" if exponent_is_one(&args[1]) => s[0].clone(),
                    "pow" => format!("pow(abs({}), {})", s[0], s[1]),
                    _ => format!("{name}({}, {})", s[0], s[1]),
                };
                Ok(V::new(out, ty))
            }
            "lerp" | "smoothstep" | "clamp" | "mad" => {
                self.need(name, args, 3)?;
                let vs = self.args_v(args)?;
                let keep_int = name == "clamp" && vs.iter().all(|v| v.ty.sc() == Some(Sc::I));
                let (s, mut ty) = self.unify_all(&vs, !keep_int)?;
                if keep_int {
                    ty = Ty::vec(Sc::I, ty.dim());
                }
                let out = match name {
                    "lerp" => format!("mix({}, {}, {})", s[0], s[1], s[2]),
                    "mad" => format!("({} * {} + {})", s[0], s[1], s[2]),
                    _ => format!("{name}({}, {}, {})", s[0], s[1], s[2]),
                };
                Ok(V::new(out, ty))
            }
            // ---- vector math ----
            "length" => {
                self.need(name, args, 1)?;
                let v = self.expr(&args[0])?;
                let ty = Ty::vec(Sc::F, v.ty.dim());
                let s = self.conv(&v, &ty)?;
                Ok(V::new(
                    if ty.dim() == 1 {
                        format!("abs({s})")
                    } else {
                        format!("length({s})")
                    },
                    Ty::F,
                ))
            }
            "normalize" => {
                self.need(name, args, 1)?;
                let v = self.expr(&args[0])?;
                let ty = Ty::vec(Sc::F, v.ty.dim());
                let s = self.conv(&v, &ty)?;
                Ok(V::new(
                    if ty.dim() == 1 {
                        format!("sign({s})")
                    } else {
                        format!("normalize({s})")
                    },
                    ty,
                ))
            }
            "distance" | "dot" => {
                self.need(name, args, 2)?;
                let vs = self.args_v(args)?;
                let (s, ty) = self.unify_all(&vs, true)?;
                let out = match (name, ty.dim()) {
                    ("dot", 1) => format!("({} * {})", s[0], s[1]),
                    ("distance", 1) => format!("abs({} - {})", s[0], s[1]),
                    _ => format!("{name}({}, {})", s[0], s[1]),
                };
                Ok(V::new(out, Ty::F))
            }
            "cross" | "reflect" => {
                self.need(name, args, 2)?;
                let vs = self.args_v(args)?;
                let (s, ty) = self.unify_all(&vs, true)?;
                if name == "cross" && ty != Ty::vec(Sc::F, 3) {
                    return terr("cross() needs float3 arguments");
                }
                Ok(V::new(format!("{name}({}, {})", s[0], s[1]), ty))
            }
            "refract" => {
                self.need(name, args, 3)?;
                let vs = self.args_v(args)?;
                let (s, ty) = self.unify_all(&vs[..2], true)?;
                let eta = self.conv(&vs[2], &Ty::F)?;
                Ok(V::new(format!("refract({}, {}, {eta})", s[0], s[1]), ty))
            }
            "any" | "all" => {
                self.need(name, args, 1)?;
                let v = self.expr(&args[0])?;
                let bt = Ty::vec(Sc::B, v.ty.dim());
                let s = self.conv(&v, &bt)?;
                Ok(V::new(
                    if bt.dim() == 1 {
                        s
                    } else {
                        format!("{name}({s})")
                    },
                    Ty::B,
                ))
            }
            "transpose" => {
                self.need(name, args, 1)?;
                let v = self.expr(&args[0])?;
                match v.ty {
                    Ty::Mat(r, c) => Ok(V::new(format!("transpose({})", v.s), Ty::Mat(c, r))),
                    _ => terr("transpose() needs a matrix"),
                }
            }
            "mul" => {
                self.need(name, args, 2)?;
                let vs = self.args_v(args)?;
                self.mul(vs[0].clone(), vs[1].clone())
            }
            "sincos" => {
                self.need(name, args, 3)?;
                let x = self.expr(&args[0])?;
                let ty = Ty::vec(Sc::F, x.ty.dim());
                let xs = self.conv(&x, &ty)?;
                let t = self.bind(V::new(xs, ty.clone()));
                let (ls, lc) = (self.lvalue(&args[1])?, self.lvalue(&args[2])?);
                let s = self.conv(&V::new(format!("sin({})", t.s), ty.clone()), &ls.ty)?;
                self.store(&ls, &s)?;
                let c = self.conv(&V::new(format!("cos({})", t.s), ty), &lc.ty)?;
                self.store(&lc, &c)?;
                Ok(V::new("", Ty::Void))
            }
            "modf" => {
                self.need(name, args, 2)?;
                let x = self.expr(&args[0])?;
                let ty = Ty::vec(Sc::F, x.ty.dim());
                let xs = self.conv(&x, &ty)?;
                let t = self.freeze(V::new(xs, ty.clone()));
                let lv = self.lvalue(&args[1])?;
                let ip = self.conv(&V::new(format!("trunc({})", t.s), ty.clone()), &lv.ty)?;
                self.store(&lv, &ip)?;
                Ok(V::new(format!("({} - trunc({}))", t.s, t.s), ty))
            }
            "clip" => {
                self.need(name, args, 1)?;
                let v = self.expr(&args[0])?;
                let ty = Ty::vec(Sc::F, v.ty.dim());
                let s = self.conv(&v, &ty)?;
                let cond = if ty.dim() == 1 {
                    format!("{s} < 0.0f")
                } else {
                    format!("any({s} < {}(0.0f))", wgsl_ty(&ty))
                };
                self.open(format!("if ({cond}) {{"));
                self.line("discard;");
                self.close();
                Ok(V::new("", Ty::Void))
            }
            _ => Err(TranslateError::Unsupported(format!(
                "unknown function or intrinsic `{name}`"
            ))),
        }
    }

    fn mul(&mut self, a: V, b: V) -> R<V> {
        match (&a.ty, &b.ty) {
            (Ty::Sc(_), _) | (_, Ty::Sc(_)) => {
                // hlslparser writes `mul(a, b)` as a plain `((a)*(b))`, not `mult()`
                self.plain_mul = true;
                let v = self.binary(BinOp::Mul, a, b);
                self.plain_mul = false;
                v
            }
            (Ty::Vec(..), Ty::Vec(..)) => {
                let (s, _ty) = self.unify_all(&[a, b], true)?;
                Ok(V::new(format!("dot({}, {})", s[0], s[1]), Ty::F))
            }
            (Ty::Vec(_, n), Ty::Mat(r, c)) => {
                // HLSL implicitly truncates the matrix (drops trailing rows) when the vector is shorter,
                // e.g. mul(float3, float4x3) == mul(float3, (float3x3)m)
                let (mv, r2) = if n < r {
                    (self.conv_v(&b, &Ty::Mat(*n, *c))?, *n)
                } else {
                    (b.clone(), *r)
                };
                let s = self.conv(&a, &Ty::vec(Sc::F, r2))?;
                Ok(V::new(format!("({s} * {})", mv.s), Ty::vec(Sc::F, *c)))
            }
            (Ty::Mat(r, c), Ty::Vec(_, n)) => {
                let (mv, c2) = if n < c {
                    (self.conv_v(&a, &Ty::Mat(*r, *n))?, *n)
                } else {
                    (a.clone(), *c)
                };
                let s = self.conv(&b, &Ty::vec(Sc::F, c2))?;
                Ok(V::new(format!("({} * {s})", mv.s), Ty::vec(Sc::F, *r)))
            }
            (Ty::Mat(r, k1), Ty::Mat(k2, c)) if k1 == k2 => {
                Ok(V::new(format!("({} * {})", a.s, b.s), Ty::Mat(*r, *c)))
            }
            _ => terr(format!(
                "mul({}, {}) not supported",
                a.ty.name(),
                b.ty.name()
            )),
        }
    }

    fn tex_lookup(&mut self, name: &str, args: &[Expr], is3d: bool, kind: &str) -> R<V> {
        let want = if kind == "grad" { 4 } else { 2 };
        self.need(name, args, want)?;
        let samp = self.sampler_of(&args[0], is3d)?;
        if self.is_fft_sampler(&samp) {
            if is3d {
                return terr("`sampler_fft` is a 2D texture: `tex3D` is not valid on it");
            }
            return self.fft_lookup(&samp, args, kind);
        }
        let cdim: u8 = if is3d { 3 } else { 2 };
        let t = format!("tex_{samp}");
        let s = format!("smp_{samp}");
        let out = V::new("", Ty::vec(Sc::F, 4));
        let cv = self.expr(&args[1])?;
        let res = match kind {
            "" | "grad" => {
                let c = self.conv(&cv, &Ty::vec(Sc::F, cdim))?;
                if kind == "grad" {
                    let dx = self.expr(&args[2])?;
                    let dy = self.expr(&args[3])?;
                    let dxs = self.conv(&dx, &Ty::vec(Sc::F, cdim))?;
                    let dys = self.conv(&dy, &Ty::vec(Sc::F, cdim))?;
                    format!("textureSampleGrad({t}, {s}, {c}, {dxs}, {dys})")
                } else {
                    format!("textureSample({t}, {s}, {c})")
                }
            }
            _ => {
                let c4 = self.conv(&cv, &Ty::vec(Sc::F, 4))?;
                let c = self.bind(V::new(c4, Ty::vec(Sc::F, 4)));
                let xyz = if is3d { "xyz" } else { "xy" };
                match kind {
                    "lod" => format!(
                        "textureSampleLevel({t}, {s}, {}.{xyz}, {}.w)",
                        paren(&c.s),
                        paren(&c.s)
                    ),
                    "bias" => format!(
                        "textureSampleBias({t}, {s}, {}.{xyz}, {}.w)",
                        paren(&c.s),
                        paren(&c.s)
                    ),
                    _ => format!(
                        "textureSample({t}, {s}, {}.{xyz} / {}.w)",
                        paren(&c.s),
                        paren(&c.s)
                    ),
                }
            }
        };
        Ok(V::new(res, out.ty))
    }
}

/// `hlslparser` (projectM) replaces `pow(x, 1)` / `pow(x, 1.0)` with `x` when the exponent is a literal one.
fn exponent_is_one(e: &Expr) -> bool {
    match e {
        Expr::Float(v) => *v == 1.0,
        Expr::Int(v) => *v == 1,
        Expr::Bool(b) => *b,
        _ => false,
    }
}
