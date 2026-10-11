//! Recursive-descent parser for the HLSL SM2/SM3 subset used by MilkDrop shaders.

use crate::TranslateError;
use crate::ast::*;
use crate::lex::{T, Tok};
use std::collections::HashMap;

pub struct Parser {
    t: Vec<Tok>,
    i: usize,
    typedefs: HashMap<String, Ty>,
    pub prog: Program,
    /// Parse like projectM's hlslparser (see [`Parser::bin_expr`]): `(float2(..) ..)` priority reset, casts swallow the
    /// rest of the expression, `?:` arms are binary expressions. Off = textbook HLSL precedence.
    pub pm_flavor: bool,
}

type R<X> = Result<X, TranslateError>;

fn perr<X>(msg: impl Into<String>) -> R<X> {
    Err(TranslateError::Parse(msg.into()))
}

const MODIFIERS: &[&str] = &[
    "static",
    "const",
    "uniform",
    "extern",
    "volatile",
    "shared",
    "precise",
    "row_major",
    "column_major",
    "inline",
    "linear",
    "centroid",
    "nointerpolation",
    "noperspective",
    "sample",
    "in",
    "out",
    "inout",
    "snorm",
    "unorm",
    "groupshared",
];

#[derive(Default, Clone, Copy)]
struct Mods {
    dir: Option<Dir>,
}

impl Parser {
    pub fn new(t: Vec<Tok>) -> Parser {
        Parser {
            t,
            i: 0,
            typedefs: HashMap::new(),
            prog: Program::default(),
            pm_flavor: false,
        }
    }

    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn peek_at(&self, k: usize) -> Option<&Tok> {
        self.t.get(self.i + k)
    }
    fn is_p(&self, p: &str) -> bool {
        self.peek().map(|t| t.is(p)).unwrap_or(false)
    }
    fn is_p_at(&self, k: usize, p: &str) -> bool {
        self.peek_at(k).map(|t| t.is(p)).unwrap_or(false)
    }
    fn is_kw(&self, k: &str) -> bool {
        self.peek().and_then(|t| t.ident()) == Some(k)
    }
    fn eat(&mut self, p: &str) -> bool {
        if self.is_p(p) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, p: &str) -> R<()> {
        if self.eat(p) {
            Ok(())
        } else {
            perr(format!("expected `{p}`, found {}", self.describe()))
        }
    }
    fn describe(&self) -> String {
        match self.peek() {
            None => "end of input".into(),
            Some(t) => match &t.t {
                T::Ident(s) => format!("`{s}`"),
                T::Int(v) => format!("`{v}`"),
                T::Float(v) => format!("`{v}`"),
                T::Punct(p) => format!("`{p}`"),
            },
        }
    }
    fn ident(&mut self) -> R<String> {
        match self.peek().and_then(|t| t.ident()) {
            Some(s) => {
                let s = s.to_string();
                self.i += 1;
                Ok(s)
            }
            None => perr(format!("expected identifier, found {}", self.describe())),
        }
    }

    fn type_of_name(&self, n: &str) -> Option<Ty> {
        if let Some(t) = self.typedefs.get(n) {
            return Some(t.clone());
        }
        builtin_type(n)
    }

    fn skip_attrs(&mut self) {
        while self.is_p("[") {
            // attribute like [unroll] / [loop] / [unroll(4)]
            let mut depth = 0;
            while let Some(t) = self.peek() {
                if t.is("[") {
                    depth += 1;
                } else if t.is("]") {
                    depth -= 1;
                    if depth == 0 {
                        self.i += 1;
                        break;
                    }
                }
                self.i += 1;
            }
        }
    }

    fn parse_mods(&mut self) -> Mods {
        let mut m = Mods::default();
        while let Some(n) = self.peek().and_then(|t| t.ident()) {
            if !MODIFIERS.contains(&n) {
                break;
            }
            match n {
                "static" | "const" => {}
                "in" => m.dir = Some(Dir::In),
                "out" => m.dir = Some(Dir::Out),
                "inout" => m.dir = Some(Dir::InOut),
                _ => {}
            }
            self.i += 1;
        }
        m
    }

    /// Try to parse a type name (including `vector<float,3>` / `matrix<float,3,3>`).
    fn try_type(&mut self) -> R<Option<Ty>> {
        let name = match self.peek().and_then(|t| t.ident()) {
            Some(n) => n.to_string(),
            None => return Ok(None),
        };
        if name == "vector" && self.is_p_at(1, "<") {
            self.i += 2;
            let sc = self.ident()?;
            let sc = builtin_type(&sc).and_then(|t| t.sc()).unwrap_or(Sc::F);
            self.expect(",")?;
            let n = self.int_lit()?;
            self.expect(">")?;
            return Ok(Some(Ty::vec(sc, n as u8)));
        }
        if name == "matrix" && self.is_p_at(1, "<") {
            self.i += 2;
            self.ident()?;
            self.expect(",")?;
            let r = self.int_lit()?;
            self.expect(",")?;
            let c = self.int_lit()?;
            self.expect(">")?;
            return Ok(Some(Ty::Mat(r as u8, c as u8)));
        }
        if let Some(t) = self.type_of_name(&name) {
            self.i += 1;
            return Ok(Some(t));
        }
        Ok(None)
    }

    fn int_lit(&mut self) -> R<i64> {
        match self.peek().map(|t| t.t.clone()) {
            Some(T::Int(v)) => {
                self.i += 1;
                Ok(v)
            }
            _ => perr(format!(
                "expected integer literal, found {}",
                self.describe()
            )),
        }
    }

    // ---------- top level ----------
    pub fn parse_program(&mut self) -> R<()> {
        while self.peek().is_some() {
            if self.eat(";") {
                continue;
            }
            self.parse_global()?;
        }
        Ok(())
    }

    fn parse_global(&mut self) -> R<()> {
        if self.is_kw("typedef") {
            self.i += 1;
            let _ = self.parse_mods();
            let ty = self
                .try_type()?
                .ok_or_else(|| TranslateError::Parse("typedef: expected type".into()))?;
            let name = self.ident()?;
            self.expect(";")?;
            self.typedefs.insert(name, ty);
            return Ok(());
        }
        if self.is_kw("struct")
            || self.is_kw("cbuffer")
            || self.is_kw("technique")
            || self.is_kw("tbuffer")
        {
            return Err(TranslateError::Unsupported(format!(
                "{} declarations",
                self.peek().and_then(|t| t.ident()).unwrap_or("")
            )));
        }
        self.skip_attrs();
        self.parse_mods();
        let ty = match self.try_type()? {
            Some(t) => t,
            None => return perr(format!("expected declaration, found {}", self.describe())),
        };
        let name = self.ident()?;
        if self.is_p("(") {
            // function
            self.i += 1;
            let mut params = Vec::new();
            if self.is_kw("void") && self.is_p_at(1, ")") {
                self.i += 1;
            }
            while !self.is_p(")") {
                let pm = self.parse_mods();
                let pty = self.try_type()?.ok_or_else(|| {
                    TranslateError::Parse(format!(
                        "expected parameter type, found {}",
                        self.describe()
                    ))
                })?;
                let pname = self.ident()?;
                let pty = self.array_suffix(pty)?;
                self.skip_semantic();
                params.push(Param {
                    name: pname,
                    ty: pty,
                    dir: pm.dir.unwrap_or(Dir::In),
                });
                if !self.eat(",") {
                    break;
                }
            }
            self.expect(")")?;
            self.skip_semantic();
            if self.eat(";") {
                return Ok(()); // forward declaration
            }
            self.expect("{")?;
            let body = self.block_rest()?;
            self.prog.funcs.push(Func {
                name,
                ret: ty,
                params,
                body,
            });
            return Ok(());
        }
        // global variable(s)
        let mut first = Some(name);
        loop {
            let name = match first.take() {
                Some(n) => n,
                None => self.ident()?,
            };
            let vty = self.array_suffix(ty.clone())?;
            self.skip_semantic();
            let init = if self.eat("=") {
                self.initializer()?
            } else {
                None
            };
            self.prog.globals.push(VarDecl {
                name,
                ty: vty,
                init,
            });
            if !self.eat(",") {
                break;
            }
        }
        self.expect(";")?;
        Ok(())
    }

    /// `: SEMANTIC`, `: register(..)`, `< annotations >`
    fn skip_semantic(&mut self) {
        loop {
            if self.is_p(":") && self.peek_at(1).and_then(|t| t.ident()).is_some() {
                self.i += 2;
                if self.is_p("(") {
                    let mut d = 0;
                    while let Some(t) = self.peek() {
                        if t.is("(") {
                            d += 1;
                        } else if t.is(")") {
                            d -= 1;
                            if d == 0 {
                                self.i += 1;
                                break;
                            }
                        }
                        self.i += 1;
                    }
                }
            } else if self.is_p("<") {
                while let Some(t) = self.peek() {
                    let end = t.is(">");
                    self.i += 1;
                    if end {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn array_suffix(&mut self, ty: Ty) -> R<Ty> {
        let mut dims = Vec::new();
        while self.is_p("[") {
            self.i += 1;
            let n = if self.is_p("]") {
                0
            } else {
                self.const_int_expr()?
            };
            self.expect("]")?;
            dims.push(n.max(0) as usize);
        }
        let mut t = ty;
        for d in dims.into_iter().rev() {
            t = Ty::Arr(Box::new(t), d);
        }
        Ok(t)
    }

    fn const_int_expr(&mut self) -> R<i64> {
        let e = self.assign_expr()?;
        fn ev(e: &Expr) -> Option<i64> {
            Some(match e {
                Expr::Int(v) => *v,
                Expr::Float(v) => *v as i64,
                Expr::Binary(op, a, b) => {
                    let (a, b) = (ev(a)?, ev(b)?);
                    match op {
                        BinOp::Add => a + b,
                        BinOp::Sub => a - b,
                        BinOp::Mul => a * b,
                        BinOp::Div if b != 0 => a / b,
                        _ => return None,
                    }
                }
                Expr::Unary(UnOp::Neg, a) => -ev(a)?,
                _ => return None,
            })
        }
        ev(&e).ok_or_else(|| TranslateError::Unsupported("non-constant array size".into()))
    }

    /// Initializer after `=`. A `sampler_state { ... }` block yields `None`.
    fn initializer(&mut self) -> R<Option<Expr>> {
        if self.is_kw("sampler_state") || self.is_kw("SamplerState") {
            self.i += 1;
            if self.is_p("{") {
                let mut d = 0;
                while let Some(t) = self.peek() {
                    if t.is("{") {
                        d += 1;
                    } else if t.is("}") {
                        d -= 1;
                        if d == 0 {
                            self.i += 1;
                            break;
                        }
                    }
                    self.i += 1;
                }
            }
            return Ok(None);
        }
        if self.is_p("{") {
            return Ok(Some(self.init_list()?));
        }
        Ok(Some(self.assign_expr()?))
    }

    fn init_list(&mut self) -> R<Expr> {
        self.expect("{")?;
        let mut v = Vec::new();
        while !self.is_p("}") {
            if self.is_p("{") {
                v.push(self.init_list()?);
            } else {
                v.push(self.assign_expr()?);
            }
            if !self.eat(",") {
                break;
            }
        }
        self.expect("}")?;
        Ok(Expr::InitList(v))
    }

    // ---------- statements ----------
    fn block_rest(&mut self) -> R<Vec<Stmt>> {
        let mut v = Vec::new();
        while !self.is_p("}") {
            if self.peek().is_none() {
                return perr("unexpected end of input in block");
            }
            v.push(self.stmt()?);
        }
        self.expect("}")?;
        Ok(v)
    }

    fn is_decl_start(&self) -> bool {
        let n = match self.peek().and_then(|t| t.ident()) {
            Some(n) => n,
            None => return false,
        };
        if MODIFIERS.contains(&n) {
            return true;
        }
        if self.type_of_name(n).is_some()
            || ((n == "vector" || n == "matrix") && self.is_p_at(1, "<"))
        {
            // `float3(...)` is a constructor expression, `float3 x` a declaration
            return !self.is_p_at(1, "(");
        }
        false
    }

    fn decl_stmt(&mut self) -> R<Stmt> {
        self.parse_mods();
        let ty = self.try_type()?.ok_or_else(|| {
            TranslateError::Parse(format!("expected type, found {}", self.describe()))
        })?;
        let mut v = Vec::new();
        loop {
            let name = self.ident()?;
            let vty = self.array_suffix(ty.clone())?;
            self.skip_semantic();
            let init = if self.eat("=") {
                self.initializer()?
            } else {
                None
            };
            v.push(VarDecl {
                name,
                ty: vty,
                init,
            });
            if !self.eat(",") {
                break;
            }
        }
        self.expect(";")?;
        Ok(Stmt::Decl(v))
    }

    fn stmt(&mut self) -> R<Stmt> {
        self.skip_attrs();
        if self.eat(";") {
            return Ok(Stmt::Empty);
        }
        if self.eat("{") {
            return Ok(Stmt::Block(self.block_rest()?));
        }
        if let Some(kw) = self.peek().and_then(|t| t.ident()).map(|s| s.to_string()) {
            match kw.as_str() {
                "if" => {
                    self.i += 1;
                    self.expect("(")?;
                    let c = self.expr()?;
                    self.expect(")")?;
                    let a = self.stmt()?;
                    let b = if self.is_kw("else") {
                        self.i += 1;
                        Some(Box::new(self.stmt()?))
                    } else {
                        None
                    };
                    return Ok(Stmt::If(c, Box::new(a), b));
                }
                "for" => {
                    self.i += 1;
                    self.expect("(")?;
                    let init = if self.eat(";") {
                        None
                    } else if self.is_decl_start() {
                        Some(Box::new(self.decl_stmt()?))
                    } else {
                        let e = self.expr()?;
                        self.expect(";")?;
                        Some(Box::new(Stmt::Expr(e)))
                    };
                    let cond = if self.is_p(";") {
                        None
                    } else {
                        Some(self.expr()?)
                    };
                    self.expect(";")?;
                    let step = if self.is_p(")") {
                        None
                    } else {
                        Some(self.expr()?)
                    };
                    self.expect(")")?;
                    let body = self.stmt()?;
                    return Ok(Stmt::For(init, cond, step, Box::new(body)));
                }
                "while" => {
                    self.i += 1;
                    self.expect("(")?;
                    let c = self.expr()?;
                    self.expect(")")?;
                    let b = self.stmt()?;
                    return Ok(Stmt::While(c, Box::new(b)));
                }
                "do" => {
                    self.i += 1;
                    let b = self.stmt()?;
                    if !self.is_kw("while") {
                        return perr("expected `while` after do-body");
                    }
                    self.i += 1;
                    self.expect("(")?;
                    let c = self.expr()?;
                    self.expect(")")?;
                    self.expect(";")?;
                    return Ok(Stmt::DoWhile(Box::new(b), c));
                }
                "return" => {
                    self.i += 1;
                    if self.eat(";") {
                        return Ok(Stmt::Return(None));
                    }
                    let e = self.expr()?;
                    self.expect(";")?;
                    return Ok(Stmt::Return(Some(e)));
                }
                "break" => {
                    self.i += 1;
                    self.expect(";")?;
                    return Ok(Stmt::Break);
                }
                "continue" => {
                    self.i += 1;
                    self.expect(";")?;
                    return Ok(Stmt::Continue);
                }
                "discard" => {
                    self.i += 1;
                    self.expect(";")?;
                    return Ok(Stmt::Discard);
                }
                "switch" => return Err(TranslateError::Unsupported("switch statement".into())),
                "struct" => return Err(TranslateError::Unsupported("struct declaration".into())),
                _ => {}
            }
        }
        if self.is_decl_start() {
            return self.decl_stmt();
        }
        let e = self.expr()?;
        self.expect(";")?;
        Ok(Stmt::Expr(e))
    }

    // ---------- expressions ----------
    pub fn expr(&mut self) -> R<Expr> {
        let e = self.assign_expr()?;
        if self.is_p(",") {
            let mut v = vec![e];
            while self.eat(",") {
                v.push(self.assign_expr()?);
            }
            return Ok(Expr::Comma(v));
        }
        Ok(e)
    }

    fn assign_expr(&mut self) -> R<Expr> {
        let l = self.bin_expr(0)?;
        let op = match self.peek() {
            Some(Tok { t: T::Punct(p), .. }) => match *p {
                "=" => Some(None),
                "+=" => Some(Some(BinOp::Add)),
                "-=" => Some(Some(BinOp::Sub)),
                "*=" => Some(Some(BinOp::Mul)),
                "/=" => Some(Some(BinOp::Div)),
                "%=" => Some(Some(BinOp::Rem)),
                "&=" => Some(Some(BinOp::BitAnd)),
                "|=" => Some(Some(BinOp::BitOr)),
                "^=" => Some(Some(BinOp::BitXor)),
                "<<=" => Some(Some(BinOp::Shl)),
                ">>=" => Some(Some(BinOp::Shr)),
                _ => None,
            },
            _ => None,
        };
        if let Some(op) = op {
            self.i += 1;
            let r = self.assign_expr()?;
            return Ok(Expr::Assign(op, Box::new(l), Box::new(r)));
        }
        Ok(l)
    }

    /// `AcceptBinaryOperator(priority)` of projectM's hlslparser: consume the next token if it is a binary
    /// operator whose priority is strictly above `priority`.
    fn accept_binop(&mut self, priority: u8) -> Option<(BinOp, u8)> {
        let (op, prec) = match self.peek() {
            Some(Tok { t: T::Punct(p), .. }) => match *p {
                "||" => (BinOp::Or, 1),
                "&&" => (BinOp::And, 2),
                "|" => (BinOp::BitOr, 3),
                "^" => (BinOp::BitXor, 4),
                "&" => (BinOp::BitAnd, 5),
                "==" => (BinOp::Eq, 6),
                "!=" => (BinOp::Ne, 6),
                "<" => (BinOp::Lt, 7),
                ">" => (BinOp::Gt, 7),
                "<=" => (BinOp::Le, 7),
                ">=" => (BinOp::Ge, 7),
                "<<" => (BinOp::Shl, 8),
                ">>" => (BinOp::Shr, 8),
                "+" => (BinOp::Add, 9),
                "-" => (BinOp::Sub, 9),
                "*" => (BinOp::Mul, 10),
                "/" => (BinOp::Div, 10),
                "%" => (BinOp::Rem, 10),
                _ => return None,
            },
            _ => return None,
        };
        if prec > priority {
            self.i += 1;
            Some((op, prec))
        } else {
            None
        }
    }

    /// Port of projectM's hlslparser `ParseBinaryExpression` *including its quirks* (projectM compiles with it):
    ///
    /// * a `(` directly followed by a type constructor (`(float2(..)*a + b)`, not a cast and not `(a + b)`) leaves
    ///   the closing `)` pending and resets the priority to 0, so after the `)` the loop keeps swallowing *every*
    ///   following operator whatever its priority: `x * (float2(a,b) + 1) - c` is `x * ((float2(a,b) + 1) - c)`;
    /// * `?:` is only recognised at priority 0.
    fn bin_expr(&mut self, mut priority: u8) -> R<Expr> {
        let (mut e, mut pending_end) = self.terminal()?;
        if pending_end {
            priority = 0;
        }
        let mut pending_op: Option<(BinOp, u8)> = None;
        loop {
            let op = match pending_op.take() {
                Some(o) => Some(o),
                None => self.accept_binop(priority),
            };
            if let Some((op, prec)) = op {
                let r = self.bin_expr(prec)?;
                e = Expr::Binary(op, Box::new(e), Box::new(r));
            } else if priority < 1 && self.eat("?") {
                // hlslparser parses both arms with `ParseBinaryExpression(1)` (no nested `?:`, no assignment), so
                // `a ? b : c ? d : e` is `(a ? b : c) ? d : e`; textbook HLSL takes assignment expressions
                let (a, b) = if self.pm_flavor {
                    let a = self.bin_expr(1)?;
                    self.expect(":")?;
                    (a, self.bin_expr(1)?)
                } else {
                    let a = self.assign_expr()?;
                    self.expect(":")?;
                    (a, self.assign_expr()?)
                };
                e = Expr::Cond(Box::new(e), Box::new(a), Box::new(b));
            } else {
                if pending_end {
                    self.expect(")")?;
                    pending_end = false;
                    if let Some(o) = self.accept_binop(priority) {
                        pending_op = Some(o);
                        continue;
                    }
                }
                break;
            }
            if let Some(o) = self.accept_binop(priority) {
                pending_op = Some(o);
                continue;
            }
            if pending_end {
                self.expect(")")?;
                pending_end = false;
            }
        }
        Ok(e)
    }

    /// `ParseTerminalExpression`: prefix operators, casts, `(float2(...` (returns with `true` = the closing `)` is
    /// still to be consumed by [`Self::bin_expr`]) and postfix chains.
    fn terminal(&mut self) -> R<(Expr, bool)> {
        if let Some(Tok { t: T::Punct(p), .. }) = self.peek() {
            let p = *p;
            match p {
                "-" => {
                    self.i += 1;
                    let (e, end) = self.terminal()?;
                    // fold negative literals
                    return Ok((
                        match e {
                            Expr::Float(v) => Expr::Float(-v),
                            Expr::Int(v) => Expr::Int(-v),
                            e => Expr::Unary(UnOp::Neg, Box::new(e)),
                        },
                        end,
                    ));
                }
                "+" => {
                    self.i += 1;
                    let (e, end) = self.terminal()?;
                    return Ok((Expr::Unary(UnOp::Plus, Box::new(e)), end));
                }
                "!" => {
                    self.i += 1;
                    let (e, end) = self.terminal()?;
                    return Ok((Expr::Unary(UnOp::Not, Box::new(e)), end));
                }
                "~" => {
                    self.i += 1;
                    let (e, end) = self.terminal()?;
                    return Ok((Expr::Unary(UnOp::BitNot, Box::new(e)), end));
                }
                "++" | "--" => {
                    self.i += 1;
                    let (e, end) = self.terminal()?;
                    return Ok((
                        Expr::IncDec {
                            inc: p == "++",
                            prefix: true,
                            e: Box::new(e),
                        },
                        end,
                    ));
                }
                "(" => {
                    if let Some(name) = self.peek_at(1).and_then(|t| t.ident())
                        && (self.type_of_name(name).is_some()
                            || name == "vector"
                            || name == "matrix")
                        && !MODIFIERS.contains(&name)
                    {
                        let save = self.i;
                        self.i += 1;
                        if let Some(ty) = self.try_type()? {
                            if self.pm_flavor && self.eat("(") {
                                // `(float2(...` : the constructor is the whole terminal, the `)` stays pending
                                let args = self.args()?;
                                return Ok((Expr::Ctor(ty, args), true));
                            }
                            if self.eat(")") {
                                // hlslparser: a cast takes the whole rest of the expression (`ParseExpression`), so
                                // `(int)a + b` is `(int)(a + b)`; textbook HLSL casts the unary operand only
                                let e = if self.pm_flavor {
                                    self.assign_expr()?
                                } else {
                                    self.terminal()?.0
                                };
                                return Ok((Expr::Cast(ty, Box::new(e)), false));
                            }
                        }
                        self.i = save;
                    }
                }
                _ => {}
            }
        }
        Ok((self.postfix()?, false))
    }

    fn postfix(&mut self) -> R<Expr> {
        let mut e = self.primary()?;
        loop {
            if self.eat(".") {
                let m = self.ident()?;
                e = Expr::Member(Box::new(e), m);
            } else if self.eat("[") {
                let i = self.expr()?;
                self.expect("]")?;
                e = Expr::Index(Box::new(e), Box::new(i));
            } else if self.is_p("++") || self.is_p("--") {
                let inc = self.is_p("++");
                self.i += 1;
                e = Expr::IncDec {
                    inc,
                    prefix: false,
                    e: Box::new(e),
                };
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn primary(&mut self) -> R<Expr> {
        let t = match self.peek() {
            Some(t) => t.t.clone(),
            None => return perr("unexpected end of input in expression"),
        };
        match t {
            T::Int(v) => {
                self.i += 1;
                Ok(Expr::Int(v))
            }
            T::Float(v) => {
                self.i += 1;
                Ok(Expr::Float(v))
            }
            T::Punct("(") => {
                self.i += 1;
                let e = self.expr()?;
                self.expect(")")?;
                Ok(e)
            }
            T::Punct("{") => self.init_list(),
            T::Ident(name) => {
                if name == "true" || name == "false" {
                    self.i += 1;
                    return Ok(Expr::Bool(name == "true"));
                }
                // constructor?
                if ((name == "vector" || name == "matrix") && self.is_p_at(1, "<")
                    || (self.type_of_name(&name).is_some() && self.is_p_at(1, "(")))
                    && let Some(ty) = self.try_type()?
                {
                    self.expect("(")?;
                    let args = self.args()?;
                    return Ok(Expr::Ctor(ty, args));
                }
                self.i += 1;
                if self.is_p("(") {
                    self.i += 1;
                    let args = self.args()?;
                    return Ok(Expr::Call(name, args));
                }
                Ok(Expr::Ident(name))
            }
            _ => perr(format!("unexpected {} in expression", self.describe())),
        }
    }

    fn args(&mut self) -> R<Vec<Expr>> {
        let mut v = Vec::new();
        if self.eat(")") {
            return Ok(v);
        }
        loop {
            v.push(self.assign_expr()?);
            if self.eat(",") {
                continue;
            }
            self.expect(")")?;
            break;
        }
        Ok(v)
    }
}
