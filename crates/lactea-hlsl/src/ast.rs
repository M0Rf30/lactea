//! HLSL types and AST for the supported ps_2/ps_3 subset.

use lactea_abi::TexDim;

/// Scalar kinds. On ps_2/ps_3 hardware ints are floats, so `I` values are f32 in WGSL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sc {
    F,
    I,
    B,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    Void,
    Sc(Sc),
    /// n in 2..=4
    Vec(Sc, u8),
    /// rows, cols (float only)
    Mat(u8, u8),
    Arr(Box<Ty>, usize),
    Sampler(TexDim),
    Texture,
}

impl Ty {
    pub const F: Ty = Ty::Sc(Sc::F);
    pub const I: Ty = Ty::Sc(Sc::I);
    pub const B: Ty = Ty::Sc(Sc::B);
    pub fn vec(sc: Sc, n: u8) -> Ty {
        if n == 1 { Ty::Sc(sc) } else { Ty::Vec(sc, n) }
    }
    pub fn dim(&self) -> u8 {
        match self {
            Ty::Sc(_) => 1,
            Ty::Vec(_, n) => *n,
            _ => 0,
        }
    }
    pub fn sc(&self) -> Option<Sc> {
        match self {
            Ty::Sc(s) | Ty::Vec(s, _) => Some(*s),
            Ty::Mat(..) => Some(Sc::F),
            _ => None,
        }
    }
    pub fn is_numeric_like(&self) -> bool {
        matches!(self, Ty::Sc(_) | Ty::Vec(..))
    }
    pub fn name(&self) -> String {
        match self {
            Ty::Void => "void".into(),
            Ty::Sc(s) => sc_name(*s).into(),
            Ty::Vec(s, n) => format!("{}{n}", sc_name(*s)),
            Ty::Mat(r, c) => format!("float{r}x{c}"),
            Ty::Arr(t, n) => format!("{}[{n}]", t.name()),
            Ty::Sampler(TexDim::D2) => "sampler2D".into(),
            Ty::Sampler(TexDim::D3) => "sampler3D".into(),
            Ty::Texture => "texture".into(),
        }
    }
}

fn sc_name(s: Sc) -> &'static str {
    match s {
        Sc::F => "float",
        Sc::I => "int",
        Sc::B => "bool",
    }
}

/// Parse a builtin HLSL type name.
pub fn builtin_type(name: &str) -> Option<Ty> {
    let (sc, rest) = if let Some(r) = name.strip_prefix("float") {
        (Sc::F, r)
    } else if let Some(r) = name.strip_prefix("half") {
        (Sc::F, r)
    } else if let Some(r) = name.strip_prefix("double") {
        (Sc::F, r)
    } else if let Some(r) = name.strip_prefix("uint") {
        (Sc::I, r)
    } else if let Some(r) = name.strip_prefix("int") {
        (Sc::I, r)
    } else if let Some(r) = name.strip_prefix("dword") {
        (Sc::I, r)
    } else if let Some(r) = name.strip_prefix("bool") {
        (Sc::B, r)
    } else {
        return match name {
            "void" => Some(Ty::Void),
            "sampler" | "sampler2D" | "sampler1D" | "SamplerState" => Some(Ty::Sampler(TexDim::D2)),
            "sampler3D" => Some(Ty::Sampler(TexDim::D3)),
            "texture" | "texture2D" | "texture3D" => Some(Ty::Texture),
            _ => None,
        };
    };
    let b = rest.as_bytes();
    match b.len() {
        0 => Some(Ty::Sc(sc)),
        1 if (b'1'..=b'4').contains(&b[0]) => Some(Ty::vec(sc, b[0] - b'0')),
        3 if (b'1'..=b'4').contains(&b[0]) && b[1] == b'x' && (b'1'..=b'4').contains(&b[2]) => {
            let (r, c) = (b[0] - b'0', b[2] - b'0');
            if r == 1 && c == 1 {
                Some(Ty::Sc(sc))
            } else if r == 1 {
                Some(Ty::vec(sc, c))
            } else if c == 1 {
                Some(Ty::vec(sc, r))
            } else {
                Some(Ty::Mat(r, c))
            }
        }
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Plus,
    Not,
    BitNot,
}

#[derive(Clone, Debug)]
pub enum Expr {
    Float(f64),
    Int(i64),
    Bool(bool),
    Ident(String),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Assign(Option<BinOp>, Box<Expr>, Box<Expr>),
    IncDec {
        inc: bool,
        prefix: bool,
        e: Box<Expr>,
    },
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    Comma(Vec<Expr>),
    Call(String, Vec<Expr>),
    Ctor(Ty, Vec<Expr>),
    Cast(Ty, Box<Expr>),
    Index(Box<Expr>, Box<Expr>),
    Member(Box<Expr>, String),
    InitList(Vec<Expr>),
}

#[derive(Clone, Debug)]
pub struct VarDecl {
    pub name: String,
    pub ty: Ty,
    pub init: Option<Expr>,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Block(Vec<Stmt>),
    Decl(Vec<VarDecl>),
    Expr(Expr),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    For(Option<Box<Stmt>>, Option<Expr>, Option<Expr>, Box<Stmt>),
    While(Expr, Box<Stmt>),
    DoWhile(Box<Stmt>, Expr),
    Return(Option<Expr>),
    Break,
    Continue,
    Discard,
    Empty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    In,
    Out,
    InOut,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub ty: Ty,
    pub dir: Dir,
}

#[derive(Clone, Debug)]
pub struct Func {
    pub name: String,
    pub ret: Ty,
    pub params: Vec<Param>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug, Default)]
pub struct Program {
    /// Global variables / samplers in declaration order.
    pub globals: Vec<VarDecl>,
    pub funcs: Vec<Func>,
}
