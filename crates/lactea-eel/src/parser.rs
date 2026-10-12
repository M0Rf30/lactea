//! Parser + compiler front end: a precedence-climbing re-implementation of the Bison grammar in
//! Compiler.y (same operator levels / associativity), building the evaluation tree with the
//! same constant folding and dead-instruction elimination as CompilerFunctions.c.

use crate::context::{CompileError, Context, Program};
use crate::lexer::{Tok, Token, lex};
use crate::ns::Dialect;
use crate::ns_pre;
use crate::tree::{
    BITAND_DEF, BITOR_DEF, FuncDef, FuncKind, Loc, Node, lookup_func, lookup_func_ns, to_ns,
};

/// Default maximum tree depth accepted (the interpreter is recursive).
pub(crate) const DEFAULT_MAX_DEPTH: u32 = 1000;

/// `prjm_eval_compiler_node_t`
struct CNode {
    node: Node,
    is_list: bool,
    /// `instr_is_const_expr`
    ic: bool,
    /// `instr_is_state_changing`
    is: bool,
    /// `list_is_const_expr`
    lc: bool,
    /// `list_is_state_changing`
    ls: bool,
    hazard: bool,
    depth: u32,
}

struct Parser<'a> {
    ctx: &'a mut Context,
    toks: Vec<Token>,
    pos: usize,
    next_slot: u32,
    rdepth: u32,
    /// Compile for `Dialect::NsEel2`.
    ns: bool,
}

type PResult<T> = Result<T, CompileError>;

fn tok_desc(t: &Tok) -> String {
    match t {
        Tok::Num(v) => format!("number {v}"),
        Tok::Var(n) => format!("variable \"{n}\""),
        Tok::Func(n) => format!("function \"{n}\""),
        Tok::Gmem => "\"gmem\"".into(),
        Tok::AddOp => "\"+=\"".into(),
        Tok::SubOp => "\"-=\"".into(),
        Tok::ModOp => "\"%=\"".into(),
        Tok::OrOp => "\"|=\"".into(),
        Tok::AndOp => "\"&=\"".into(),
        Tok::DivOp => "\"/=\"".into(),
        Tok::MulOp => "\"*=\"".into(),
        Tok::PowOp => "\"^=\"".into(),
        Tok::Equal => "\"==\"".into(),
        Tok::BelEq => "\"<=\"".into(),
        Tok::AboEq => "\">=\"".into(),
        Tok::NotEq => "\"!=\"".into(),
        Tok::BoolOr => "\"||\"".into(),
        Tok::BoolAnd => "\"&&\"".into(),
        Tok::Ch(c) => format!("'{}'", *c as char),
        Tok::Undef(c) => format!("invalid character {:?}", *c as char),
        Tok::Eof => "end of input".into(),
    }
}

/// (precedence, right-assoc, function name) of an infix token.  Levels follow the `%left` /
/// `%right` declaration order in Compiler.y, lowest first.
fn infix(t: &Tok) -> Option<(u8, bool, &'static str)> {
    Some(match t {
        Tok::Ch(b'=') => (2, true, "_set"),
        Tok::AddOp => (3, true, "_addop"),
        Tok::SubOp => (3, true, "_subop"),
        Tok::MulOp => (4, true, "_mulop"),
        Tok::DivOp => (4, true, "_divop"),
        Tok::ModOp => (4, true, "_modop"),
        Tok::PowOp => (5, true, "_powop"),
        Tok::OrOp => (5, true, "_orop"),
        Tok::AndOp => (5, true, "_andop"),
        Tok::Ch(b'?') => (6, true, "?"),
        Tok::BoolOr => (7, false, "_or"),
        Tok::BoolAnd => (8, false, "_and"),
        Tok::Ch(b'|') => (9, false, "/*or*/"),
        Tok::Ch(b'&') => (10, false, "/*and*/"),
        Tok::Equal => (11, false, "_equal"),
        Tok::NotEq => (11, false, "_noteq"),
        Tok::Ch(b'>') => (12, false, "_above"),
        Tok::AboEq => (12, false, "_aboeq"),
        Tok::Ch(b'<') => (13, false, "_below"),
        Tok::BelEq => (13, false, "_beleq"),
        Tok::Ch(b'+') => (14, false, "_add"),
        Tok::Ch(b'-') => (14, false, "_sub"),
        Tok::Ch(b'*') => (15, false, "_mul"),
        Tok::Ch(b'/') => (15, false, "_div"),
        Tok::Ch(b'%') => (16, false, "_mod"),
        Tok::Ch(b'^') => (18, false, "pow"),
        _ => return None,
    })
}

fn starts_expr(t: &Tok) -> bool {
    matches!(
        t,
        Tok::Num(_)
            | Tok::Var(_)
            | Tok::Func(_)
            | Tok::Gmem
            | Tok::Ch(b'(')
            | Tok::Ch(b'-')
            | Tok::Ch(b'+')
            | Tok::Ch(b'!')
    )
}

impl<'a> Parser<'a> {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }

    fn peek_at(&self, off: usize) -> &Tok {
        let i = (self.pos + off).min(self.toks.len() - 1);
        &self.toks[i].tok
    }

    fn err_at(&self, idx: usize, msg: String) -> CompileError {
        let t = &self.toks[idx.min(self.toks.len() - 1)];
        CompileError {
            message: msg,
            line: t.line,
            column: t.col,
            column_end: t.col + t.len,
        }
    }

    fn unexpected(&self) -> CompileError {
        self.err_at(
            self.pos,
            format!("syntax error, unexpected {}", tok_desc(self.peek())),
        )
    }

    /// Minimum binding level of a unary `-` / `+` operand: projectm-eval binds the sign tighter
    /// than everything (`-2^2` is `(-2)^2`).
    fn unary_level(&self) -> u8 {
        20
    }

    fn expect(&mut self, c: u8) -> PResult<()> {
        if *self.peek() == Tok::Ch(c) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.unexpected())
        }
    }

    fn alloc(&mut self) -> u32 {
        let s = self.next_slot;
        self.next_slot += 4;
        s
    }

    // ---- node construction (CompilerFunctions.c) ----

    fn constant(&self, v: f64) -> CNode {
        CNode {
            node: Node::Const(v),
            is_list: false,
            ic: true,
            is: false,
            lc: true,
            ls: false,
            hazard: false,
            depth: 1,
        }
    }

    fn variable(&mut self, name: &str) -> CNode {
        let v = self.ctx.var(name);
        CNode {
            node: Node::Var(self.ctx.loc_of(v)),
            is_list: false,
            ic: false,
            is: false,
            lc: false,
            ls: false,
            hazard: false,
            depth: 1,
        }
    }

    fn named(&mut self, name: &str, args: Vec<CNode>, at: usize) -> PResult<CNode> {
        let def = match name {
            "/*or*/" => BITOR_DEF,
            "/*and*/" => BITAND_DEF,
            _ => lookup_func(name).expect("operator/function names passed here are in the table"),
        };
        let def = if self.ns { to_ns(def) } else { def };
        self.func(def, name, args, at)
    }

    /// `prjm_eval_compiler_create_function` + `_create_expression`.
    fn func(&mut self, def: FuncDef, name: &str, args: Vec<CNode>, at: usize) -> PResult<CNode> {
        if def.argc != args.len() {
            return Err(self.err_at(
                at,
                format!(
                    "Invalid argument count for function \"{}\": Expected {}, but {} given.",
                    name,
                    def.argc,
                    args.len()
                ),
            ));
        }
        let args_const = args.iter().all(|a| a.lc);
        let args_state = args.iter().any(|a| a.ls);
        let depth = 1 + args.iter().map(|a| a.depth).max().unwrap_or(0);
        if depth > self.ctx.max_depth {
            return Err(self.err_at(at, "expression nested too deeply".into()));
        }
        let hazard_args = args.iter().any(|a| a.hazard);
        let b_hazard = args.get(1).map(|a| a.hazard).unwrap_or(false);
        let a_place = args.first().map(|a| a.node.is_place()).unwrap_or(false);
        let ns = self.ns;

        let mut it = args.into_iter().map(|a| a.node);
        let mut next = || {
            it.next()
                .expect("arity was checked against the function definition")
        };
        let node = match def.kind {
            FuncKind::If => {
                let s = self.alloc();
                Node::If {
                    s,
                    args: Box::new([next(), next(), next()]),
                    ns,
                }
            }
            FuncKind::And => Node::And(Box::new([next(), next()]), ns),
            FuncKind::Or => Node::Or(Box::new([next(), next()]), ns),
            FuncKind::Loop => {
                let s = self.alloc();
                Node::Loop {
                    s,
                    args: Box::new([next(), next()]),
                    ns,
                }
            }
            FuncKind::While => {
                let s = self.alloc();
                Node::While {
                    s,
                    arg: Box::new(next()),
                    ns,
                }
            }
            FuncKind::Un(op) => Node::Un(op, Box::new(next())),
            FuncKind::Bin(op) => Node::Bin {
                op,
                args: Box::new([next(), next()]),
                defer: a_place && b_hazard,
            },
            FuncKind::Assign(op) => {
                let s = self.alloc();
                Node::OpAssign {
                    op,
                    s,
                    args: Box::new([next(), next()]),
                }
            }
            FuncKind::Set => {
                let s = self.alloc();
                Node::Set {
                    s,
                    args: Box::new([next(), next()]),
                    filter: ns,
                }
            }
            FuncKind::Exec2 => {
                let s = self.alloc();
                Node::Exec2 {
                    s,
                    args: Box::new([next(), next()]),
                }
            }
            FuncKind::Exec3 => {
                let s = self.alloc();
                Node::Exec3 {
                    s,
                    args: Box::new([next(), next(), next()]),
                }
            }
            FuncKind::Mem { global } => {
                let s = self.alloc();
                Node::Mem {
                    global,
                    ns,
                    s,
                    arg: Box::new(next()),
                }
            }
            FuncKind::FreeMbuf => Node::FreeMbuf {
                arg: Box::new(next()),
            },
            FuncKind::Memcpy => {
                let s = self.alloc();
                Node::Memcpy {
                    s,
                    args: Box::new([next(), next(), next()]),
                    ns,
                }
            }
            FuncKind::Memset => {
                let s = self.alloc();
                Node::Memset {
                    s,
                    args: Box::new([next(), next(), next()]),
                    ns,
                }
            }
        };

        let ic = args_const && def.is_const;
        let is = args_state || def.is_state_changing;
        let hazard = hazard_args || node.kind_hazard();
        if ic && !is {
            // Compile-time evaluation of constant subtrees.
            let v = self.fold(&node);
            return Ok(CNode {
                node: Node::Const(v),
                is_list: false,
                ic: true,
                is: false,
                lc: true,
                ls: false,
                hazard: false,
                depth: 1,
            });
        }
        Ok(CNode {
            node,
            is_list: false,
            ic,
            is,
            lc: ic,
            ls: is,
            hazard,
            depth,
        })
    }

    fn fold(&mut self, node: &Node) -> f64 {
        self.ctx.ensure_scratch(self.next_slot as usize);
        let l = self.ctx.place(node, Loc::Scratch(0));
        self.ctx.read(l)
    }

    /// `prjm_eval_compiler_add_instruction`
    fn add_instruction(&mut self, list: CNode, instr: CNode) -> CNode {
        let mut node = if !list.is_list {
            if !list.is {
                // A preceding non-state-changing expression does nothing useful.
                return instr;
            }
            let s = self.alloc();
            CNode {
                node: Node::List {
                    s,
                    items: vec![list.node],
                },
                is_list: true,
                ic: list.ic,
                is: list.is,
                lc: list.lc,
                ls: list.ls,
                hazard: list.hazard,
                depth: list.depth + 1,
            }
        } else {
            list
        };
        if let Node::List { items, .. } = &mut node.node {
            if items.len() >= 2 && !node.is {
                items.pop();
            }
            items.push(instr.node);
        }
        node.ic = instr.lc;
        node.is = instr.ls;
        node.lc = node.lc && instr.lc;
        node.ls = node.ls || instr.ls;
        node.hazard = node.hazard || instr.hazard;
        node.depth = node.depth.max(instr.depth + 1);
        node
    }

    // ---- grammar ----

    fn parse_instr_list(&mut self) -> PResult<CNode> {
        let mut list = self.parse_expr(0)?;
        while *self.peek() == Tok::Ch(b';') {
            self.pos += 1;
            match self.peek() {
                Tok::Ch(b';') | Tok::Ch(b',') | Tok::Ch(b')') | Tok::Eof => continue,
                Tok::Ch(b'(') if *self.peek_at(1) == Tok::Ch(b')') => {
                    self.pos += 2;
                }
                t if starts_expr(t) => {
                    let e = self.parse_expr(0)?;
                    list = self.add_instruction(list, e);
                }
                _ => return Err(self.unexpected()),
            }
        }
        Ok(list)
    }

    fn parse_expr(&mut self, min: u8) -> PResult<CNode> {
        self.rdepth += 1;
        if self.rdepth > self.ctx.max_depth {
            return Err(self.err_at(self.pos, "expression nested too deeply".into()));
        }
        let r = self.parse_expr_inner(min);
        self.rdepth -= 1;
        r
    }

    fn parse_expr_inner(&mut self, min: u8) -> PResult<CNode> {
        let mut lhs = self.parse_prefix()?;
        loop {
            let at = self.pos;
            let t = self.peek().clone();
            if t == Tok::Ch(b'[') {
                self.pos += 1;
                let idx = if *self.peek() == Tok::Ch(b']') {
                    self.pos += 1;
                    lhs
                } else {
                    let off = self.parse_expr(0)?;
                    self.expect(b']')?;
                    self.named("_add", vec![lhs, off], at)?
                };
                lhs = self.named("_mem", vec![idx], at)?;
                continue;
            }
            let Some((prec, right, name)) = infix(&t) else {
                break;
            };
            if prec < min {
                break;
            }
            self.pos += 1;
            if name == "?" {
                let tv = self.parse_expr(0)?;
                self.expect(b':')?;
                let fv = self.parse_expr(6)?;
                lhs = self.named("_if", vec![lhs, tv, fv], at)?;
            } else {
                let rhs = self.parse_expr(if right { prec } else { prec + 1 })?;
                lhs = self.named(name, vec![lhs, rhs], at)?;
            }
        }
        Ok(lhs)
    }

    fn parse_prefix(&mut self) -> PResult<CNode> {
        let at = self.pos;
        let t = self.peek().clone();
        match t {
            Tok::Num(v) => {
                self.pos += 1;
                Ok(self.constant(v))
            }
            Tok::Var(name) => {
                self.pos += 1;
                Ok(self.variable(&name))
            }
            Tok::Func(name) => {
                self.pos += 1;
                self.expect(b'(')?;
                let mut args = vec![self.parse_instr_list()?];
                while *self.peek() == Tok::Ch(b',') {
                    self.pos += 1;
                    args.push(self.parse_instr_list()?);
                }
                self.expect(b')')?;
                let def = lookup_func(&name).expect("lexer only emits Tok::Func for known names");
                self.func(def, &name, args, at)
            }
            Tok::Gmem => {
                self.pos += 1;
                self.expect(b'[')?;
                let idx = if *self.peek() == Tok::Ch(b']') {
                    self.constant(0.0)
                } else {
                    self.parse_expr(0)?
                };
                self.expect(b']')?;
                self.named("_gmem", vec![idx], at)
            }
            Tok::Ch(b'(') => {
                self.pos += 1;
                let n = self.parse_instr_list()?;
                self.expect(b')')?;
                Ok(n)
            }
            Tok::Ch(b'-') => {
                self.pos += 1;
                let v = self.parse_expr(self.unary_level())?;
                self.named("_neg", vec![v], at)
            }
            Tok::Ch(b'+') => {
                self.pos += 1;
                self.parse_expr(self.unary_level())
            }
            Tok::Ch(b'!') => {
                self.pos += 1;
                let v = self.parse_expr(18)?;
                self.named("_not", vec![v], at)
            }
            _ => Err(self.unexpected()),
        }
    }
}

/// ns-eel2: the source rewriter (`ns_pre`) has already turned every `= ?: && || ==... % ^ [ ] !`
/// into a call, so what is left is the grammar of `nseel-caltab.c`: `| & + - * / %` over unary
/// minus, calls and parentheses, one expression per `;`-separated segment.
impl<'a> Parser<'a> {
    /// Binding level of the infix tokens left after the rewrite.  `%` is the sequencing operator
    /// (the compiler's `FN_MODULO` entry is `exec2`; the real modulo is the rewritten `_mod`).
    fn ns_infix(t: &Tok) -> Option<(u8, &'static str)> {
        Some(match t {
            Tok::Ch(b'|') => (1, "/*or*/"),
            Tok::Ch(b'&') => (2, "/*and*/"),
            Tok::Ch(b'+') => (3, "_add"),
            Tok::Ch(b'-') => (3, "_sub"),
            Tok::Ch(b'*') => (4, "_mul"),
            Tok::Ch(b'/') => (4, "_div"),
            Tok::Ch(b'%') => (4, "exec2"),
            _ => return None,
        })
    }

    fn ns_expr(&mut self, min: u8) -> PResult<CNode> {
        self.rdepth += 1;
        if self.rdepth > self.ctx.max_depth {
            return Err(self.err_at(self.pos, "expression nested too deeply".into()));
        }
        let r = self.ns_expr_inner(min);
        self.rdepth -= 1;
        r
    }

    fn ns_expr_inner(&mut self, min: u8) -> PResult<CNode> {
        let mut lhs = self.ns_prefix()?;
        while let Some((prec, name)) = Self::ns_infix(self.peek()) {
            if prec < min {
                break;
            }
            let at = self.pos;
            self.pos += 1;
            let rhs = self.ns_expr(prec + 1)?;
            lhs = self.named(name, vec![lhs, rhs], at)?;
        }
        Ok(lhs)
    }

    fn ns_prefix(&mut self) -> PResult<CNode> {
        let at = self.pos;
        match self.peek().clone() {
            Tok::Num(v) => {
                self.pos += 1;
                Ok(self.constant(v))
            }
            Tok::Var(name) => {
                self.pos += 1;
                Ok(self.variable(&name))
            }
            Tok::Func(name) => {
                self.pos += 1;
                self.expect(b'(')?;
                let mut args = vec![self.ns_expr(0)?];
                while *self.peek() == Tok::Ch(b',') {
                    self.pos += 1;
                    args.push(self.ns_expr(0)?);
                }
                self.expect(b')')?;
                let def =
                    lookup_func_ns(&name).expect("lexer only emits Tok::Func for known names");
                self.func(def, &name, args, at)
            }
            Tok::Ch(b'(') => {
                self.pos += 1;
                let n = self.ns_expr(0)?;
                self.expect(b')')?;
                Ok(n)
            }
            // `UMINUS` / `UPLUS` bind tighter than every infix operator.
            Tok::Ch(b'-') => {
                self.pos += 1;
                self.rdepth += 1;
                if self.rdepth > self.ctx.max_depth {
                    return Err(self.err_at(self.pos, "expression nested too deeply".into()));
                }
                let v = self.ns_prefix();
                self.rdepth -= 1;
                self.named("_neg", vec![v?], at)
            }
            Tok::Ch(b'+') => {
                self.pos += 1;
                self.rdepth += 1;
                if self.rdepth > self.ctx.max_depth {
                    return Err(self.err_at(self.pos, "expression nested too deeply".into()));
                }
                let v = self.ns_prefix();
                self.rdepth -= 1;
                v
            }
            _ => Err(self.unexpected()),
        }
    }

    /// `NSEEL_code_compile`: each `;`-separated segment is parsed on its own and the first one
    /// that fails rejects the whole text.
    fn ns_program(&mut self) -> PResult<Option<CNode>> {
        let mut list: Option<CNode> = None;
        loop {
            while *self.peek() == Tok::Ch(b';') {
                self.pos += 1;
            }
            if *self.peek() == Tok::Eof {
                return Ok(list);
            }
            let e = self.ns_expr(0)?;
            if !matches!(self.peek(), Tok::Ch(b';') | Tok::Eof) {
                return Err(self.unexpected());
            }
            list = Some(match list {
                None => e,
                Some(l) => self.add_instruction(l, e),
            });
        }
    }
}

pub(crate) fn compile(ctx: &mut Context, src: &str) -> Result<Program, CompileError> {
    let ns = ctx.dialect() == Dialect::NsEel2;
    let rewritten;
    let src = if ns {
        rewritten =
            ns_pre::preprocess(src.as_bytes(), ctx.max_depth).map_err(|()| CompileError {
                message: "expression nested too deeply".into(),
                line: 1,
                column: 0,
                column_end: 0,
            })?;
        String::from_utf8_lossy(&rewritten)
    } else {
        std::borrow::Cow::Borrowed(src)
    };
    let toks = lex(&src, ns);
    // Undefined characters are scanner errors regardless of where the parser stands.
    let mut p = Parser {
        ctx,
        toks,
        pos: 0,
        next_slot: 4,
        rdepth: 0,
        ns,
    };
    let root = if ns {
        match p.ns_program()? {
            Some(r) => r,
            None => return Ok(Program::new(None, 4, p.ctx.id())),
        }
    } else {
        if *p.peek() == Tok::Eof {
            return Ok(Program::new(None, 4, p.ctx.id()));
        }
        let root = p.parse_instr_list()?;
        if *p.peek() != Tok::Eof {
            return Err(p.unexpected());
        }
        root
    };
    let n = p.next_slot;
    let id = p.ctx.id();
    Ok(Program::new(Some(crate::fast::lower(root.node)), n, id))
}
