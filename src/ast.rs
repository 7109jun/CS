use crate::lexer::{Span, TplPart};
use serde_json::Value as Json;
use std::rc::Rc;

#[derive(Debug, Clone)]
pub enum TypeExpr {
    Named { name: String, args: Vec<TypeExpr>, span: Span },
    Array(Box<TypeExpr>),
    Tuple(Vec<TypeExpr>),
    Object(Vec<PropSig>, Span),
    Union(Vec<TypeExpr>),
    Func { tparams: Vec<TypeParam>, params: Vec<FnTypeParam>, ret: Box<TypeExpr>, span: Span },
    LitStr(String),
    LitNum(f64),
    LitBool(bool),
    Null,
}

#[derive(Debug, Clone)]
pub struct FnTypeParam {
    pub name: String,
    pub ty: TypeExpr,
    pub optional: bool,
    pub rest: bool,
}

#[derive(Debug, Clone)]
pub struct PropSig {
    pub name: String,
    pub ty: TypeExpr,
    pub optional: bool,
    /// `[key: string]: T` index signature
    pub index: bool,
}

#[derive(Debug, Clone)]
pub struct TypeParam {
    pub name: String,
    pub constraint: Option<TypeExpr>,
}

#[derive(Debug, Clone)]
pub enum Pattern {
    Ident(String, Span),
    Object { props: Vec<PatProp>, rest: Option<String>, span: Span },
    Array { elems: Vec<Option<PatElem>>, rest: Option<String>, span: Span },
}

#[derive(Debug, Clone)]
pub struct PatProp {
    pub key: String,
    pub target: Pattern,
    pub default: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct PatElem {
    pub target: Pattern,
    pub default: Option<Expr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogOp {
    And,
    Or,
    Nullish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Plus,
    Not,
    BitNot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Assign,
    Bin(BinOp),
    Log(LogOp),
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ArrElem {
    Item(Expr),
    Spread(Expr),
}

#[derive(Debug, Clone)]
pub enum ObjProp {
    KV(String, Expr),
    Spread(Expr),
}

#[derive(Debug, Clone)]
pub enum Arg {
    Item(Expr),
    Spread(Expr),
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Num(f64),
    Str(String),
    Template(Vec<TemplateSeg>),
    Bool(bool),
    Null,
    Ident(String),
    Array(Vec<ArrElem>),
    Object(Vec<ObjProp>),
    Func(Rc<FuncDecl>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Logical(LogOp, Box<Expr>, Box<Expr>),
    Assign { op: AssignOp, target: Box<Expr>, value: Box<Expr> },
    Update { inc: bool, prefix: bool, target: Box<Expr> },
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    Call { callee: Box<Expr>, targs: Vec<TypeExpr>, args: Vec<Arg>, optional: bool },
    Member { obj: Box<Expr>, name: String, optional: bool },
    Index { obj: Box<Expr>, idx: Box<Expr>, optional: bool },
    /// Wraps a whole optional chain `a?.b.c` so a short-circuit skips the rest of it.
    Chain(Box<Expr>),
    Await(Box<Expr>),
    TypeOf(Box<Expr>),
    As(Box<Expr>, TypeExpr),
    NonNull(Box<Expr>),
}

#[derive(Debug, Clone)]
pub enum TemplateSeg {
    Str(String),
    Expr(Expr),
}

pub fn tpl_part_unused(_: &TplPart) {}

#[derive(Debug, Clone)]
pub struct Param {
    pub pattern: Pattern,
    pub ty: Option<TypeExpr>,
    pub optional: bool,
    pub default: Option<Expr>,
    pub rest: bool,
}

#[derive(Debug, Clone)]
pub struct Decorator {
    pub name: String,
    pub args: Vec<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone, Default)]
pub struct FuncMeta {
    pub input_schema: Option<Json>,
    pub output_schema: Option<Json>,
    /// "passthrough" | "text" | "structured" | "json" | "void" (tools); "string" | "content" | "contents" (resources); "string" | "messages" (prompts)
    pub result_kind: String,
    pub param_names: Vec<String>,
    pub param_required: Vec<bool>,
    /// Constant decorator options evaluated by the checker, one per decorator.
    pub decorator_opts: Vec<(String, Vec<Json>)>,
}

#[derive(Debug)]
pub struct FuncDecl {
    pub name: Option<String>,
    pub tparams: Vec<TypeParam>,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub body: Vec<Stmt>,
    pub is_async: bool,
    pub is_arrow: bool,
    pub decorators: Vec<Decorator>,
    pub exported: bool,
    pub span: Span,
    /// Filled by the type checker for decorated functions.
    pub meta: std::cell::RefCell<FuncMeta>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarKind {
    Let,
    Const,
}

#[derive(Debug, Clone)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ImportSpec {
    pub name: String,
    pub alias: String,
}

#[derive(Debug, Clone)]
pub struct Case {
    pub test: Option<Expr>,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub struct InterfaceDecl {
    pub name: String,
    pub tparams: Vec<TypeParam>,
    pub extends: Vec<TypeExpr>,
    pub props: Vec<PropSig>,
    pub exported: bool,
}

#[derive(Debug, Clone)]
pub enum StmtKind {
    Var { kind: VarKind, pattern: Pattern, ty: Option<TypeExpr>, init: Option<Expr>, exported: bool },
    Expr(Expr),
    Block(Vec<Stmt>),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    While(Expr, Box<Stmt>),
    DoWhile(Box<Stmt>, Expr),
    For { init: Option<Box<Stmt>>, cond: Option<Expr>, update: Option<Expr>, body: Box<Stmt> },
    ForOf { kind: VarKind, pattern: Pattern, iter: Expr, body: Box<Stmt> },
    Switch(Expr, Vec<Case>),
    Return(Option<Expr>),
    Break,
    Continue,
    Throw(Expr),
    Try { block: Vec<Stmt>, catch: Option<(Option<Pattern>, Vec<Stmt>)>, finally: Option<Vec<Stmt>> },
    Func(Rc<FuncDecl>),
    TypeAlias { name: String, tparams: Vec<TypeParam>, ty: TypeExpr, exported: bool },
    Interface(InterfaceDecl),
    Import { specs: Vec<ImportSpec>, ns: Option<String>, from: String, type_only: bool },
    /// `export { a, b as c }`
    ExportNames(Vec<ImportSpec>),
    DeclareFunc { name: String, tparams: Vec<TypeParam>, params: Vec<FnTypeParam>, ret: TypeExpr, exported: bool },
    DeclareConst { name: String, ty: TypeExpr, exported: bool },
    Test { name: String, body: Vec<Stmt> },
}

#[derive(Debug, Clone)]
pub struct Module {
    pub path: String,
    pub is_header: bool,
    pub body: Vec<Stmt>,
}
