//! Type-checking WGSL emitter. Walks the parsed program starting at `PS`, translating only
//! reachable functions/globals, tracking HLSL types for every expression and inserting the
//! implicit conversions HLSL performs (scalar splat, vector truncation, int/bool promotion).
//!
//! Integer model (ps_2/ps_3): ints are floats on that hardware, so HLSL `int`/`uint` values
//! are WGSL `f32`; float->int conversion truncates, int arithmetic is float arithmetic
//! (`int / int` is a float division, `%` is fmod).

mod conv;
mod expr;
pub(crate) mod fft;
mod fold;
mod intrinsics;
mod stmt;
mod undef;

use crate::ast::*;
use crate::{Prelude, TranslateError};
use lactea_abi::TexDim;
use std::collections::{HashMap, HashSet};

pub(crate) type R<X> = Result<X, TranslateError>;

pub(crate) fn terr<X>(m: impl Into<String>) -> R<X> {
    Err(TranslateError::Type(m.into()))
}

#[derive(Clone, Debug)]
pub(crate) struct V {
    pub s: String,
    pub ty: Ty,
}

impl V {
    pub fn new(s: impl Into<String>, ty: Ty) -> V {
        V { s: s.into(), ty }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Local {
    pub w: String,
    pub ty: Ty,
    /// out/inout parameter: a `ptr<function, T>`, accessed through `(*w)`.
    pub ptr: bool,
}

pub(crate) enum GKind {
    Sampler,
    /// Maps onto a field of the frozen `MilkUniforms` (`mu`) or, for the MD3 names, of `MilkExtUniforms` (`mx`).
    Mapped(String),
    /// `float4 texsize_<name>;` -> `pu.texsize[slot]`
    PassTexsize,
    Private,
}

pub(crate) struct GInfo {
    pub decl: VarDecl,
    pub order: usize,
    pub kind: GKind,
    pub wname: String,
    /// For uniforms the shader writes to: initial value expression (the real uniform).
    pub shadow: Option<String>,
    pub emitted: bool,
}

#[derive(Default)]
pub(crate) struct FnCtx {
    pub scopes: Vec<HashMap<String, Local>>,
    pub lines: Vec<String>,
    pub indent: usize,
    pub ret: Option<Ty>,
    pub is_ps: bool,
}

const SCOPE_OPEN: &str = "a function scope is always open while statements are translated";

pub(crate) struct Emitted {
    /// Everything after `WGSL_COMMON` except the texture bindings: private globals + functions + fs_main.
    pub body: String,
    /// Samplers referenced by reachable code: (hlsl name, declared order, declared dim, used with tex3D)
    pub samplers: Vec<(String, usize, TexDim, bool)>,
    /// `texsize_*` globals referenced: (hlsl name `texsize_x`, slot)
    pub texsizes: Vec<(String, usize)>,
    pub warnings: Vec<String>,
}

pub(crate) struct UserFn {
    pub idx: usize,
    pub wname: Option<String>,
}

pub(crate) struct Tr<'a> {
    pub prog: &'a Program,
    pub globals: HashMap<String, GInfo>,
    pub fns: HashMap<String, Vec<UserFn>>,
    pub cx: FnCtx,
    pub uniq: usize,
    pub fn_out: Vec<String>,
    pub private_decls: Vec<String>,
    pub init_lines: Vec<String>,
    pub used_samplers: HashMap<String, bool>, // name -> used with 3D
    pub used_texsize: Vec<String>,
    pub warnings: Vec<String>,
    /// Float `%` / `fmod` follow GLSL `mod` (`x - y*floor(x/y)`, sign of the divisor) instead of HLSL's truncated
    /// remainder (see `Options::floor_mod`).
    pub floor_mod: bool,
    /// projectM hlslparser behaviours (see `Options::projectm_flavor`): float literals rounded to 6 significant
    /// digits, asin/acos outside `[-1, 1]`.
    pub pm_flavor: bool,
    /// The shader prelude in effect (see `Options::md3` / `Options::beatdrop`): `_c14` / `_qi.._qp` map onto `mx` and
    /// `fft` / `wave` samplers are lowered to `fft_fetch` / `bd_fetch` calls.
    pub prelude: Prelude,
    /// The `*` being translated is a compound `*=` or `mul()`: hlslparser emits those as plain products (no `mult()`).
    pub plain_mul: bool,
    /// Variables Mesa compiles as NaN instead of undefined (see `undef.rs`); only filled for `Options::projectm_flavor`.
    pub undef_nan: HashSet<String>,
    /// Source expressions (by address) whose components read an extra varying: `fold` index per component
    /// (`fold.rs`, `Options::gl_varying_fold`).
    pub fold_roots: HashMap<usize, Vec<Option<usize>>>,
    /// Vertex-stage functions of the extra varyings (`Translated::folds`).
    pub folds: Vec<String>,
}

pub(crate) fn wgsl_ty(t: &Ty) -> String {
    match t {
        Ty::Void => "void".into(),
        Ty::Sc(Sc::B) => "bool".into(),
        Ty::Sc(_) => "f32".into(),
        Ty::Vec(Sc::B, n) => format!("vec{n}<bool>"),
        Ty::Vec(_, n) => format!("vec{n}<f32>"),
        Ty::Mat(r, c) => format!("mat{c}x{r}<f32>"),
        Ty::Arr(e, n) => format!("array<{}, {n}>", wgsl_ty(e)),
        Ty::Sampler(_) | Ty::Texture => "sampler".into(),
    }
}

/// A float literal as projectM's hlslparser re-emits it into GLSL: `ostream << float` prints 6 significant digits
/// (`String_FormatFloat`), the GLSL compiler then parses that text.
///
/// Rounds `v` to what the GLSL compiler parses from that printed text.
pub(crate) fn round_literal_6(v: f64) -> f64 {
    let x = v as f32;
    if !x.is_finite() || x == 0.0 {
        return x as f64;
    }
    format!("{:.5e}", x as f64)
        .parse::<f32>()
        .map_or(x as f64, |y| y as f64)
}

/// Format an `f32` as a WGSL literal. NaN becomes `0.0` and infinities saturate to `f32::MAX`/`MIN`: WGSL has
/// no non-finite literals, and MilkDrop 2 shader constants are never meant to be non-finite.
pub(crate) fn fmt_f32(v: f64) -> String {
    let mut x = v as f32;
    if x.is_nan() {
        x = 0.0;
    }
    if x.is_infinite() {
        x = if x > 0.0 { f32::MAX } else { f32::MIN };
    }
    let mut s = format!("{x:?}");
    if !s.contains('.') && !s.contains('e') && !s.contains('E') {
        s.push_str(".0");
    }
    if x.is_sign_negative() && x != 0.0 {
        format!("({s}f)")
    } else if x == 0.0 && x.is_sign_negative() {
        "0.0f".into()
    } else {
        format!("{s}f")
    }
}

pub(crate) fn is_simple(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    // identifier path (`a`, `mu.c`, `t_3.x`) or numeric literal (`1.0f`, `(-1.0f)`)
    let ident_path = |x: &str| {
        let c0 = x.chars().next();
        c0.is_some_and(|c0| c0.is_ascii_alphabetic() || c0 == '_')
            && x.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    };
    let num = |x: &str| x.strip_suffix('f').unwrap_or(x).parse::<f32>().is_ok();
    if ident_path(s) || num(s) {
        return true;
    }
    s.starts_with("(-") && s.ends_with("f)") && num(&s[2..s.len() - 1].replace(')', ""))
}

/// `s` wrapped in parentheses unless it is a plain identifier/field/index path (valid as a
/// swizzle or index base as-is).
pub(crate) fn mp(s: &str) -> String {
    let path = !s.is_empty()
        && s.chars()
            .next()
            .map(|c| c.is_ascii_alphabetic() || c == '_')
            .unwrap_or(false)
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '[' | ']'))
        && s.matches('[').count() == s.matches(']').count();
    if path {
        s.to_string()
    } else {
        format!("({s})")
    }
}

/// Names `include.fx` (and the active prelude) declares that live in `MilkUniforms` / `MilkExtUniforms`. BeatDrop's
/// `_c14.._c16` use the extension block's mouse and the first two `q_hi` registers (`q33..` do not exist there).
pub(crate) fn builtin_uniform(name: &str, prelude: impl Into<Prelude>) -> Option<String> {
    let prelude = prelude.into();
    let md3 = prelude.md3();
    if let Some(n) = name.strip_prefix("_c")
        && let Ok(i) = n.parse::<usize>()
        && i < 14
    {
        return Some(format!("mu.c[{i}]"));
    }
    if (md3 || prelude.beatdrop()) && name == "_c14" {
        return Some("mx.mouse".into());
    }
    if prelude.beatdrop() {
        match name {
            "_c15" => return Some("mx.q_hi[0]".into()),
            "_c16" => return Some("mx.q_hi[1]".into()),
            _ => {}
        }
    }
    if let Some(n) = name.strip_prefix("_q")
        && n.len() == 1
    {
        let b = n.as_bytes()[0];
        if (b'a'..=b'h').contains(&b) {
            return Some(format!("mu.q[{}]", b - b'a'));
        }
        if md3 && (b'i'..=b'p').contains(&b) {
            return Some(format!("mx.q_hi[{}]", b - b'i'));
        }
    }
    if md3
        && let Some(n) = name.strip_prefix("_mxd3q")
        && n.len() == 2
    {
        let idx = match n.as_bytes() {
            [b'x', b'a'] => 0,
            [b'x', b'b'] => 1,
            [b'x', b'c'] => 2,
            [b'x', b'd'] => 3,
            [b's', b'a'] => 4,
            [b's', b'b'] => 5,
            [b's', b'c'] => 6,
            _ => return None,
        };
        return Some(format!("mx.mxd3q[{idx}]"));
    }
    match name {
        "rand_frame" => return Some("mu.rand_frame".into()),
        "rand_preset" => return Some("mu.rand_preset".into()),
        "texsize_noise_lq" => return Some("mu.texsize_noise[0]".into()),
        "texsize_noise_lq_lite" => return Some("mu.texsize_noise[1]".into()),
        "texsize_noise_mq" => return Some("mu.texsize_noise[2]".into()),
        "texsize_noise_hq" => return Some("mu.texsize_noise[3]".into()),
        "texsize_noisevol_lq" => return Some("mu.texsize_noise[4]".into()),
        "texsize_noisevol_hq" => return Some("mu.texsize_noise[5]".into()),
        _ => {}
    }
    if let Some(r) = name.strip_prefix("rot_") {
        let (base, off) = if let Some(n) = r.strip_prefix("rand") {
            (20, n)
        } else if let Some(n) = r.strip_prefix("vf") {
            (12, n)
        } else if let Some(n) = r.strip_prefix("uf") {
            (16, n)
        } else if let Some(n) = r.strip_prefix('s') {
            (0, n)
        } else if let Some(n) = r.strip_prefix('d') {
            (4, n)
        } else {
            let n = r.strip_prefix('f')?;
            (8, n)
        };
        if let Ok(k) = off.parse::<usize>()
            && (1..=4).contains(&k)
        {
            return Some(format!("mu.rot[{}]", base + k - 1));
        }
    }
    None
}

impl<'a> Tr<'a> {
    pub fn new(prog: &'a Program, prelude: impl Into<Prelude>) -> Tr<'a> {
        let prelude = prelude.into();
        let assigned = assigned_idents(prog);
        let mut globals: HashMap<String, GInfo> = HashMap::new();
        for (order, d) in prog.globals.iter().enumerate() {
            if globals.contains_key(&d.name) {
                continue; // redeclaration (e.g. user re-declares an include.fx sampler)
            }
            let mut shadow = None;
            let kind = if matches!(d.ty, Ty::Sampler(_)) {
                GKind::Sampler
            } else if matches!(d.ty, Ty::Texture) {
                GKind::Private // unused texture object declarations
            } else if let Some(m) = builtin_uniform(&d.name, prelude) {
                if assigned.contains(&d.name) {
                    // HLSL lets shaders write to uniforms (they are per-invocation copies)
                    shadow = Some(m);
                    GKind::Private
                } else {
                    GKind::Mapped(m)
                }
            } else if d.name.starts_with("texsize_")
                && d.ty == Ty::vec(Sc::F, 4)
                && d.init.is_none()
            {
                GKind::PassTexsize
            } else {
                GKind::Private
            };
            globals.insert(
                d.name.clone(),
                GInfo {
                    decl: d.clone(),
                    order,
                    kind,
                    wname: format!("g_{}", d.name.trim_start_matches('_')),
                    emitted: false,
                    shadow,
                },
            );
        }
        let mut fns: HashMap<String, Vec<UserFn>> = HashMap::new();
        for (idx, f) in prog.funcs.iter().enumerate() {
            fns.entry(f.name.clone())
                .or_default()
                .push(UserFn { idx, wname: None });
        }
        Tr {
            prog,
            globals,
            fns,
            cx: FnCtx::default(),
            uniq: 0,
            fn_out: Vec::new(),
            private_decls: Vec::new(),
            init_lines: Vec::new(),
            used_samplers: HashMap::new(),
            used_texsize: Vec::new(),
            warnings: Vec::new(),
            floor_mod: false,
            pm_flavor: false,
            plain_mul: false,
            prelude,
            undef_nan: HashSet::new(),
            fold_roots: HashMap::new(),
            folds: Vec::new(),
        }
    }

    pub fn fresh(&mut self, base: &str) -> String {
        self.uniq += 1;
        let b: String = base
            .trim_start_matches('_')
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let b = if b.is_empty() { "v".to_string() } else { b };
        format!("{b}_{}", self.uniq)
    }

    // ---------- line emission ----------
    pub fn line(&mut self, s: impl AsRef<str>) {
        let pad = "    ".repeat(self.cx.indent + 1);
        self.cx.lines.push(format!("{pad}{}", s.as_ref()));
    }
    pub fn open(&mut self, s: impl AsRef<str>) {
        self.line(s);
        self.cx.indent += 1;
    }
    pub fn close(&mut self) {
        self.cx.indent -= 1;
        self.line("}");
    }
    pub fn close_with(&mut self, s: &str) {
        self.cx.indent -= 1;
        self.line(s);
    }

    /// Capture emitted lines of `f` instead of appending them.
    pub fn capture<X>(&mut self, f: impl FnOnce(&mut Self) -> R<X>) -> R<(Vec<String>, X)> {
        let saved = std::mem::take(&mut self.cx.lines);
        let r = f(self);
        let got = std::mem::replace(&mut self.cx.lines, saved);
        Ok((got, r?))
    }

    pub fn push_scope(&mut self) {
        self.cx.scopes.push(HashMap::new());
    }
    pub fn pop_scope(&mut self) {
        self.cx.scopes.pop();
    }
    pub fn declare(&mut self, name: &str, ty: &Ty) -> String {
        let w = self.fresh(name);
        self.cx.scopes.last_mut().expect(SCOPE_OPEN).insert(
            name.to_string(),
            Local {
                w: w.clone(),
                ty: ty.clone(),
                ptr: false,
            },
        );
        w
    }
    pub fn lookup_local(&self, name: &str) -> Option<&Local> {
        self.cx.scopes.iter().rev().find_map(|s| s.get(name))
    }

    /// Materialize a value into a `let` unless it is trivially cheap/pure.
    pub fn bind(&mut self, v: V) -> V {
        if is_simple(&v.s) {
            return v;
        }
        let t = self.fresh("t");
        self.line(format!("let {t} = {};", v.s));
        V { s: t, ty: v.ty }
    }
    /// Like [`bind`] but always copies (used to freeze a value against later mutation).
    pub fn freeze(&mut self, v: V) -> V {
        if v.s.parse::<f32>().is_ok()
            || (v.s.ends_with('f') && v.s[..v.s.len() - 1].parse::<f32>().is_ok())
            || v.s.starts_with("(-")
        {
            return v;
        }
        let t = self.fresh("t");
        self.line(format!("let {t} = {};", v.s));
        V { s: t, ty: v.ty }
    }

    // ---------- globals ----------
    /// Resolve a global identifier; emits lazily-initialised private globals.
    pub fn global_value(&mut self, name: &str) -> R<Option<V>> {
        enum Class {
            Sampler,
            Mapped(String),
            PassTexsize,
            Private,
        }
        let (class, ty, wname) = match self.globals.get(name) {
            None => return Ok(None),
            Some(g) => (
                match &g.kind {
                    GKind::Sampler => Class::Sampler,
                    GKind::Mapped(m) => Class::Mapped(m.clone()),
                    GKind::PassTexsize => Class::PassTexsize,
                    GKind::Private => Class::Private,
                },
                g.decl.ty.clone(),
                g.wname.clone(),
            ),
        };
        match class {
            Class::Sampler => {
                let dim = match ty {
                    Ty::Sampler(d) => d,
                    _ => TexDim::D2,
                };
                self.used_samplers.entry(name.to_string()).or_insert(false);
                Ok(Some(V::new(name, Ty::Sampler(dim))))
            }
            Class::Mapped(m) => Ok(Some(V::new(m, ty))),
            Class::PassTexsize => {
                let slot = match self.used_texsize.iter().position(|n| n == name) {
                    Some(p) => p,
                    None => {
                        self.used_texsize.push(name.to_string());
                        self.used_texsize.len() - 1
                    }
                };
                if slot >= lactea_abi::MAX_PASS_TEXTURES {
                    return Err(TranslateError::Unsupported(
                        "more than 16 texsize_* parameters".into(),
                    ));
                }
                Ok(Some(V::new(format!("pu.texsize[{slot}]"), ty)))
            }
            Class::Private => {
                if !self.globals[name].emitted {
                    if let Some(g) = self.globals.get_mut(name) {
                        g.emitted = true;
                    }
                    self.private_decls
                        .push(format!("var<private> {wname}: {};", wgsl_ty(&ty)));
                    let init = self.globals[name].decl.init.clone();
                    if let Some(sh) = self.globals[name].shadow.clone() {
                        self.init_lines.push(format!("    {wname} = {sh};"));
                    } else if let Some(init) = init {
                        self.emit_global_init(&wname, &ty, &init)?;
                    } else if self.undef_nan.contains(name) {
                        let nan = self.nan_value(&ty);
                        self.init_lines.push(format!("    {wname} = {nan};"));
                    }
                }
                Ok(Some(V::new(wname, ty)))
            }
        }
    }

    fn emit_global_init(&mut self, wname: &str, ty: &Ty, init: &Expr) -> R<()> {
        let saved = std::mem::take(&mut self.cx);
        self.cx.scopes.push(HashMap::new());
        self.cx.indent = 0;
        let r = (|| -> R<()> {
            let v = self.init_value(ty, init)?;
            self.line(format!("{wname} = {};", v));
            Ok(())
        })();
        let lines = std::mem::take(&mut self.cx.lines);
        self.cx = saved;
        r?;
        self.init_lines.extend(lines);
        Ok(())
    }

    // ---------- functions ----------
    pub fn translate_fn(&mut self, name: &str, idx_in_overloads: usize) -> R<String> {
        if let Some(w) = &self.fns[name][idx_in_overloads].wname {
            return Ok(w.clone());
        }
        let fidx = self.fns[name][idx_in_overloads].idx;
        let wname = format!("f_{}_{}", name.trim_start_matches('_'), fidx);
        if let Some(o) = self
            .fns
            .get_mut(name)
            .and_then(|v| v.get_mut(idx_in_overloads))
        {
            o.wname = Some(wname.clone());
        }
        let f: &'a Func = &self.prog.funcs[fidx];
        let saved = std::mem::take(&mut self.cx);
        self.cx.scopes.push(HashMap::new());
        self.cx.ret = Some(f.ret.clone());
        self.cx.is_ps = f.name == "PS";
        if self.cx.is_ps && self.pm_flavor {
            self.undef_nan = undef::nan_vars(self.prog, f, &self.globals);
        }
        let r = (|| -> R<String> {
            let mut params = Vec::new();
            let mut prologue = Vec::new();
            for p in &f.params {
                let pn = self.fresh(&format!("p_{}", p.name));
                match p.dir {
                    Dir::In => {
                        if matches!(p.ty, Ty::Sampler(_) | Ty::Texture) {
                            return Err(TranslateError::Unsupported(
                                "sampler function parameters".into(),
                            ));
                        }
                        params.push(format!("{pn}: {}", wgsl_ty(&p.ty)));
                        let ln = self.declare(&p.name, &p.ty);
                        prologue.push(format!("    var {ln}: {} = {pn};", wgsl_ty(&p.ty)));
                    }
                    Dir::Out | Dir::InOut => {
                        params.push(format!("{pn}: ptr<function, {}>", wgsl_ty(&p.ty)));
                        self.cx.scopes.last_mut().expect(SCOPE_OPEN).insert(
                            p.name.clone(),
                            Local {
                                w: pn,
                                ty: p.ty.clone(),
                                ptr: true,
                            },
                        );
                    }
                }
            }
            self.cx.lines.extend(prologue);
            self.push_scope();
            for s in &f.body {
                self.stmt(s)?;
            }
            self.pop_scope();
            if f.ret != Ty::Void && !matches!(f.body.last(), Some(Stmt::Return(_))) {
                let z = zero_value(&f.ret);
                self.line(format!("return {z};"));
            }
            let ret = if f.ret == Ty::Void {
                String::new()
            } else {
                format!(" -> {}", wgsl_ty(&f.ret))
            };
            let mut out = format!("fn {wname}({}){ret} {{\n", params.join(", "));
            for l in &self.cx.lines {
                out.push_str(l);
                out.push('\n');
            }
            out.push_str("}\n");
            Ok(out)
        })();
        self.cx = saved;
        let text = r?;
        self.fn_out.push(text);
        Ok(wname)
    }
}

pub(crate) fn zero_value(t: &Ty) -> String {
    match t {
        Ty::Sc(Sc::B) => "false".into(),
        Ty::Sc(_) => "0.0f".into(),
        _ => format!("{}()", wgsl_ty(t)),
    }
}

impl Tr<'_> {
    /// `ty` filled with NaN. WGSL has no NaN literal and rejects a const-evaluated one, so the bits go through a
    /// module-level `var` (like `div_ieee`).
    pub fn nan_value(&mut self, ty: &Ty) -> String {
        const DECL: &str = "var<private> g_nan_bits: u32 = 0x7fc00000u;";
        if !self.private_decls.iter().any(|d| d == DECL) {
            self.private_decls.push(DECL.to_string());
        }
        let n = "bitcast<f32>(g_nan_bits)";
        match ty {
            Ty::Vec(_, _) => format!("{}({n})", wgsl_ty(ty)),
            _ => n.to_string(),
        }
    }
}

impl Tr<'_> {
    /// Assemble everything after `WGSL_COMMON` (minus texture bindings) and report used resources.
    pub fn finish(&mut self, ps: &str, kind: lactea_abi::PassKind) -> Emitted {
        let mut body = String::new();
        for d in &self.private_decls {
            body.push_str(d);
            body.push('\n');
        }
        for f in &self.fn_out {
            body.push_str(f);
            body.push('\n');
        }
        body.push_str("@fragment\nfn fs_main(in: VsOut) -> @location(0) vec4<f32> {\n");
        for l in &self.init_lines {
            body.push_str(l);
            body.push('\n');
        }
        for j in 0..self.folds.len().div_ceil(4) {
            body.push_str(&format!("    g_fold{j} = in.fold{j};\n"));
        }
        body.push_str("    var rv: vec4<f32> = vec4<f32>();\n");
        let uv = if kind == lactea_abi::PassKind::Comp {
            "in.uv.xy"
        } else {
            "in.uv"
        };
        body.push_str(&format!(
            "    {ps}(in.diffuse, {uv}, in.rad_ang, &rv);\n    return rv;\n}}\n"
        ));
        let mut samplers = Vec::new();
        for (name, used3d) in &self.used_samplers {
            let g = &self.globals[name];
            let dim = match g.decl.ty {
                Ty::Sampler(d) => d,
                _ => TexDim::D2,
            };
            samplers.push((name.clone(), g.order, dim, *used3d));
        }
        let texsizes = self
            .used_texsize
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), i))
            .collect();
        Emitted {
            body,
            samplers,
            texsizes,
            warnings: std::mem::take(&mut self.warnings),
        }
    }
}

fn lvalue_root(e: &Expr) -> Option<&str> {
    match e {
        Expr::Ident(n) => Some(n),
        Expr::Member(b, _) | Expr::Index(b, _) => lvalue_root(b),
        _ => None,
    }
}

fn scan_expr(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Assign(_, l, r) => {
            if let Some(n) = lvalue_root(l) {
                out.insert(n.to_string());
            }
            scan_expr(l, out);
            scan_expr(r, out);
        }
        Expr::IncDec { e, .. } => {
            if let Some(n) = lvalue_root(e) {
                out.insert(n.to_string());
            }
            scan_expr(e, out);
        }
        Expr::Unary(_, a) | Expr::Cast(_, a) | Expr::Member(a, _) => scan_expr(a, out),
        Expr::Binary(_, a, b) | Expr::Index(a, b) => {
            scan_expr(a, out);
            scan_expr(b, out);
        }
        Expr::Cond(a, b, c) => {
            scan_expr(a, out);
            scan_expr(b, out);
            scan_expr(c, out);
        }
        Expr::Comma(v) | Expr::Ctor(_, v) | Expr::InitList(v) => {
            v.iter().for_each(|x| scan_expr(x, out))
        }
        Expr::Call(n, v) => {
            // out arguments of intrinsics
            match (n.as_str(), v.len()) {
                ("sincos", 3) => {
                    for a in &v[1..] {
                        if let Some(r) = lvalue_root(a) {
                            out.insert(r.to_string());
                        }
                    }
                }
                ("modf", 2) => {
                    if let Some(r) = lvalue_root(&v[1]) {
                        out.insert(r.to_string());
                    }
                }
                _ => {}
            }
            v.iter().for_each(|x| scan_expr(x, out))
        }
        _ => {}
    }
}

fn scan_stmt(s: &Stmt, out: &mut HashSet<String>) {
    match s {
        Stmt::Block(v) => v.iter().for_each(|x| scan_stmt(x, out)),
        Stmt::Decl(ds) => ds
            .iter()
            .filter_map(|d| d.init.as_ref())
            .for_each(|e| scan_expr(e, out)),
        Stmt::Expr(e) => scan_expr(e, out),
        Stmt::If(c, a, b) => {
            scan_expr(c, out);
            scan_stmt(a, out);
            if let Some(b) = b {
                scan_stmt(b, out);
            }
        }
        Stmt::For(i, c, st, b) => {
            if let Some(i) = i {
                scan_stmt(i, out);
            }
            if let Some(c) = c {
                scan_expr(c, out);
            }
            if let Some(st) = st {
                scan_expr(st, out);
            }
            scan_stmt(b, out);
        }
        Stmt::While(c, b) => {
            scan_expr(c, out);
            scan_stmt(b, out);
        }
        Stmt::DoWhile(b, c) => {
            scan_stmt(b, out);
            scan_expr(c, out);
        }
        Stmt::Return(Some(e)) => scan_expr(e, out),
        _ => {}
    }
}

/// Names (any scope) that are assignment targets anywhere in the program.
pub(crate) fn assigned_idents(prog: &Program) -> HashSet<String> {
    let mut out = HashSet::new();
    for f in &prog.funcs {
        f.body.iter().for_each(|s| scan_stmt(s, &mut out));
    }
    for g in &prog.globals {
        if let Some(e) = &g.init {
            scan_expr(e, &mut out);
        }
    }
    out
}
