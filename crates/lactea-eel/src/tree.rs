//! Compiled expression tree and the function table (TreeFunctions.c `intrinsic_function_table`).

/// Where a result lives.  projectm-eval functions return a *pointer* to the value they produce
/// (a variable, a memory cell, or a scratch cell inside the tree) and `=`/`+=`/... write through
/// it; `Loc` is the safe equivalent of that pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Loc {
    /// Per-program scratch cell (`ctx->value` / locals of the C implementation).
    Scratch(u32),
    /// Context variable slot.
    Var(u32),
    /// `reg00 + n` global.
    Reg(u8),
    /// Local megabuf index.
    Mem(u32),
    /// gmegabuf index.
    GMem(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnOp {
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Sqr,
    Sqrt,
    Exp,
    Neg,
    Log,
    Log10,
    Abs,
    Sign,
    Rand,
    Floor,
    Ceil,
    Invsqrt,
    Bnot,
    // ns-eel2 (`Dialect::NsEel2`) variants; the projectm-eval ones above are unchanged.
    /// `log` without the `x <= 0 -> 0` guard.
    LogRaw,
    /// `log10` without the guard.
    Log10Raw,
    /// `asin` without the domain guard (NaN outside [-1, 1]).
    AsinRaw,
    /// `acos` without the domain guard.
    AcosRaw,
    /// `sign` that keeps `-0` and maps NaN by its sign bit.
    SignNs,
    /// float32 magic-number `invsqrt`.
    InvsqrtNs,
    /// `!x` with ns-eel2 truthiness.
    BnotNs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Min,
    Max,
    Atan2,
    Sigmoid,
    Equal,
    NotEq,
    Below,
    Above,
    BelEq,
    AboEq,
    Band,
    Bor,
    BitOr,
    BitAnd,
    // ns-eel2 variants.
    /// `/` without the epsilon guard.
    DivRaw,
    /// integer `%` on `|a|`, `|b|` (unsigned, zero divisor gives 0).
    ModNs,
    /// `pow` without the zero / NaN special cases.
    PowRaw,
    /// `|a-b| < 1e-5` with x87 NaN behaviour.
    EqualNs,
    /// `|a-b| >= 1e-5`.
    NotEqNs,
    /// `|` with round-to-nearest 64-bit conversion.
    BitOrNs,
    /// `&` with round-to-nearest 64-bit conversion.
    BitAndNs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AssignOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Or,
    And,
    // ns-eel2 variants (unfiltered, raw results).
    DivRaw,
    ModNs,
    PowRaw,
    OrNs,
    AndNs,
}

#[derive(Debug)]
pub(crate) enum Node {
    Const(f64),
    Var(Loc),
    Un(UnOp, Box<Node>),
    /// `defer`: first operand returns a place and the second can modify state, so the first
    /// value must be read *after* the second operand ran (C reads through pointers).
    Bin {
        op: BinOp,
        args: Box<[Node; 2]>,
        defer: bool,
    },
    /// `ns`: ns-eel2 truthiness (`|x| >= 1e-5`) instead of projectm-eval's.
    And(Box<[Node; 2]>, bool),
    Or(Box<[Node; 2]>, bool),
    If {
        s: u32,
        args: Box<[Node; 3]>,
        ns: bool,
    },
    Exec2 {
        s: u32,
        args: Box<[Node; 2]>,
    },
    Exec3 {
        s: u32,
        args: Box<[Node; 3]>,
    },
    List {
        s: u32,
        items: Vec<Node>,
    },
    /// `ns`: the count is rounded (x87 `fistp`) instead of truncated.
    Loop {
        s: u32,
        args: Box<[Node; 2]>,
        ns: bool,
    },
    /// `ns`: ns-eel2 truthiness.
    While {
        s: u32,
        arg: Box<Node>,
        ns: bool,
    },
    /// `filter`: store NaN / Inf / denormal / -0 as +0 (ns-eel2 `=`).
    Set {
        s: u32,
        args: Box<[Node; 2]>,
        filter: bool,
    },
    OpAssign {
        op: AssignOp,
        s: u32,
        args: Box<[Node; 2]>,
    },
    /// `ns`: ns-eel2 index rules (rounded index, 8M-entry megabuf, wrapping 1M-entry gmegabuf).
    Mem {
        global: bool,
        ns: bool,
        s: u32,
        arg: Box<Node>,
    },
    FreeMbuf {
        arg: Box<Node>,
    },
    /// `ns`: truncating indices without the `+ 0.0001` of projectm-eval.
    Memcpy {
        s: u32,
        args: Box<[Node; 3]>,
        ns: bool,
    },
    Memset {
        s: u32,
        args: Box<[Node; 3]>,
        ns: bool,
    },
}

impl Node {
    /// Does evaluating this node yield a reference (place) rather than a plain value?
    pub(crate) fn is_place(&self) -> bool {
        !matches!(
            self,
            Node::Const(_) | Node::Un(..) | Node::Bin { .. } | Node::And(..) | Node::Or(..)
        )
    }

    /// Scratch slot used as the "caller local" when this node is evaluated for its value only.
    pub(crate) fn out_slot(&self) -> u32 {
        match self {
            Node::If { s, .. }
            | Node::Exec2 { s, .. }
            | Node::Exec3 { s, .. }
            | Node::List { s, .. }
            | Node::Loop { s, .. }
            | Node::While { s, .. }
            | Node::Set { s, .. }
            | Node::OpAssign { s, .. }
            | Node::Mem { s, .. }
            | Node::Memcpy { s, .. }
            | Node::Memset { s, .. } => s + 3,
            // Var / FreeMbuf never write their "out" cell; pure nodes are evaluated by value.
            _ => 0,
        }
    }

    /// Can evaluating this node change something another place-returning sibling points at
    /// (variables, memory, or, through shared out-pointers, `exec3`/`while`)?
    pub(crate) fn kind_hazard(&self) -> bool {
        matches!(
            self,
            Node::Set { .. }
                | Node::OpAssign { .. }
                | Node::Memcpy { .. }
                | Node::Memset { .. }
                | Node::Exec3 { .. }
                | Node::While { .. }
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FuncKind {
    If,
    And,
    Or,
    Loop,
    While,
    Un(UnOp),
    Bin(BinOp),
    Assign(AssignOp),
    Set,
    Exec2,
    Exec3,
    Mem { global: bool },
    FreeMbuf,
    Memcpy,
    Memset,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FuncDef {
    pub kind: FuncKind,
    pub argc: usize,
    pub is_const: bool,
    pub is_state_changing: bool,
}

const fn f(kind: FuncKind, argc: usize, is_const: bool, is_state_changing: bool) -> FuncDef {
    FuncDef {
        kind,
        argc,
        is_const,
        is_state_changing,
    }
}

/// `/*or*/` operator `|`.
pub(crate) const BITOR_DEF: FuncDef = f(FuncKind::Bin(BinOp::BitOr), 2, true, false);
/// `/*and*/` operator `&`.
pub(crate) const BITAND_DEF: FuncDef = f(FuncKind::Bin(BinOp::BitAnd), 2, true, false);

/// Longest function / operator name in the table (`_`-internals included); longer names
/// cannot match and skip the lowercase buffer.
const MAX_FUNC_NAME: usize = 16;

/// Looks up a function name (ASCII case-insensitive, like `strcasecmp` in the C locale).
/// Includes the `_`-prefixed internals which the grammar uses for operators.
pub(crate) fn lookup_func(name: &str) -> Option<FuncDef> {
    use AssignOp as A;
    use BinOp as B;
    use FuncKind as K;
    use UnOp as U;
    if name.len() > MAX_FUNC_NAME {
        return None;
    }
    let mut buf = [0u8; MAX_FUNC_NAME];
    for (i, b) in name.bytes().enumerate() {
        buf[i] = b.to_ascii_lowercase();
    }
    let lower = std::str::from_utf8(&buf[..name.len()]).ok()?;
    Some(match lower {
        "if" | "_if" => f(K::If, 3, true, false),
        "_and" => f(K::And, 2, true, false),
        "_or" => f(K::Or, 2, true, false),
        "loop" => f(K::Loop, 2, true, false),
        "while" => f(K::While, 1, true, false),

        "_not" | "bnot" => f(K::Un(U::Bnot), 1, true, false),
        "_equal" | "equal" => f(K::Bin(B::Equal), 2, true, false),
        "_noteq" => f(K::Bin(B::NotEq), 2, true, false),
        "_below" | "below" => f(K::Bin(B::Below), 2, true, false),
        "_above" | "above" => f(K::Bin(B::Above), 2, true, false),
        "_beleq" => f(K::Bin(B::BelEq), 2, true, false),
        "_aboeq" => f(K::Bin(B::AboEq), 2, true, false),

        "_set" | "assign" => f(K::Set, 2, false, true),
        "_add" => f(K::Bin(B::Add), 2, true, false),
        "_sub" => f(K::Bin(B::Sub), 2, true, false),
        "_mul" => f(K::Bin(B::Mul), 2, true, false),
        "_div" => f(K::Bin(B::Div), 2, true, false),
        "_mod" => f(K::Bin(B::Mod), 2, true, false),
        "_mulop" => f(K::Assign(A::Mul), 2, false, true),
        "_divop" => f(K::Assign(A::Div), 2, false, true),
        "_orop" => f(K::Assign(A::Or), 2, false, true),
        "_andop" => f(K::Assign(A::And), 2, false, true),
        "_addop" => f(K::Assign(A::Add), 2, false, true),
        "_subop" => f(K::Assign(A::Sub), 2, false, true),
        "_modop" => f(K::Assign(A::Mod), 2, false, true),

        "sin" => f(K::Un(U::Sin), 1, true, false),
        "cos" => f(K::Un(U::Cos), 1, true, false),
        "tan" => f(K::Un(U::Tan), 1, true, false),
        "asin" => f(K::Un(U::Asin), 1, true, false),
        "acos" => f(K::Un(U::Acos), 1, true, false),
        "atan" => f(K::Un(U::Atan), 1, true, false),
        "atan2" => f(K::Bin(B::Atan2), 2, true, false),
        "sqr" => f(K::Un(U::Sqr), 1, true, false),
        "sqrt" => f(K::Un(U::Sqrt), 1, true, false),
        "pow" => f(K::Bin(B::Pow), 2, true, false),
        "_powop" => f(K::Assign(A::Pow), 2, false, true),
        "exp" => f(K::Un(U::Exp), 1, true, false),
        "_neg" => f(K::Un(U::Neg), 1, true, false),

        "log" => f(K::Un(U::Log), 1, true, false),
        "log10" => f(K::Un(U::Log10), 1, true, false),
        "abs" => f(K::Un(U::Abs), 1, true, false),
        "min" => f(K::Bin(B::Min), 2, true, false),
        "max" => f(K::Bin(B::Max), 2, true, false),
        "sign" => f(K::Un(U::Sign), 1, true, false),
        "rand" => f(K::Un(U::Rand), 1, false, false),
        "floor" | "int" => f(K::Un(U::Floor), 1, true, false),
        "ceil" => f(K::Un(U::Ceil), 1, true, false),
        "invsqrt" => f(K::Un(U::Invsqrt), 1, true, false),
        "sigmoid" => f(K::Bin(B::Sigmoid), 2, true, false),

        "band" => f(K::Bin(B::Band), 2, true, false),
        "bor" => f(K::Bin(B::Bor), 2, true, false),

        "exec2" => f(K::Exec2, 2, true, false),
        "exec3" => f(K::Exec3, 3, true, false),
        "_mem" | "megabuf" => f(K::Mem { global: false }, 1, false, true),
        "_gmem" | "gmegabuf" => f(K::Mem { global: true }, 1, false, true),
        "freembuf" => f(K::FreeMbuf, 1, false, true),
        "memcpy" => f(K::Memcpy, 3, false, true),
        "memset" => f(K::Memset, 3, false, true),
        _ => return None,
    })
}

/// Operators the ns-eel2 compiler emits itself (JIT glue, not entries of its 53-function
/// table): they cannot be called by name, so `_add(1, 2)` is a syntax error there.
pub(crate) fn is_ns_operator_only(name: &str) -> bool {
    ["_add", "_sub", "_mul", "_div", "_neg"]
        .iter()
        .any(|n| name.eq_ignore_ascii_case(n))
}

/// Looks up a name a program may call under `Dialect::NsEel2`: the projectm-eval table minus the
/// operator-only entries, with the ns-eel2 operator kinds.
pub(crate) fn lookup_func_ns(name: &str) -> Option<FuncDef> {
    if is_ns_operator_only(name) {
        return None;
    }
    lookup_func(name).map(to_ns)
}

/// The ns-eel2 flavour of a projectm-eval function definition (same arity and const-ness).
/// Control-flow flags (`ns` fields of the nodes) are added where the node is built.
pub(crate) fn to_ns(mut def: FuncDef) -> FuncDef {
    use AssignOp as A;
    use BinOp as B;
    use UnOp as U;
    def.kind = match def.kind {
        FuncKind::Un(U::Log) => FuncKind::Un(U::LogRaw),
        FuncKind::Un(U::Log10) => FuncKind::Un(U::Log10Raw),
        FuncKind::Un(U::Asin) => FuncKind::Un(U::AsinRaw),
        FuncKind::Un(U::Acos) => FuncKind::Un(U::AcosRaw),
        FuncKind::Un(U::Sign) => FuncKind::Un(U::SignNs),
        FuncKind::Un(U::Invsqrt) => FuncKind::Un(U::InvsqrtNs),
        FuncKind::Un(U::Bnot) => FuncKind::Un(U::BnotNs),
        FuncKind::Bin(B::Div) => FuncKind::Bin(B::DivRaw),
        FuncKind::Bin(B::Mod) => FuncKind::Bin(B::ModNs),
        FuncKind::Bin(B::Pow) => FuncKind::Bin(B::PowRaw),
        FuncKind::Bin(B::Equal) => FuncKind::Bin(B::EqualNs),
        FuncKind::Bin(B::NotEq) => FuncKind::Bin(B::NotEqNs),
        FuncKind::Bin(B::BitOr) => FuncKind::Bin(B::BitOrNs),
        FuncKind::Bin(B::BitAnd) => FuncKind::Bin(B::BitAndNs),
        FuncKind::Assign(A::Div) => FuncKind::Assign(A::DivRaw),
        FuncKind::Assign(A::Mod) => FuncKind::Assign(A::ModNs),
        FuncKind::Assign(A::Pow) => FuncKind::Assign(A::PowRaw),
        FuncKind::Assign(A::Or) => FuncKind::Assign(A::OrNs),
        FuncKind::Assign(A::And) => FuncKind::Assign(A::AndNs),
        k => k,
    };
    def
}
