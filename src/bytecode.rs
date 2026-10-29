use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum Op {
    Nop,
    Const(u32),
    Null,
    True,
    False,
    Pop,
    Dup,
    /// Duplicate the top two values: [a b] -> [a b a b]
    Dup2,
    /// [a b] -> [b a]
    Swap,
    /// [a b c] -> [c a b]
    Rot3,
    GetLocal(u32),
    SetLocal(u32),
    NewCell(u32),
    GetCell(u32),
    SetCell(u32),
    RefreshCell(u32),
    GetUpval(u32),
    SetUpval(u32),
    GetGlobal(u32),
    SetGlobal(u32),
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Neg,
    Plus,
    Not,
    BitNot,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    TypeOf,
    Jump(u32),
    JumpIfFalse(u32),
    JumpIfTrue(u32),
    /// if top is falsy: keep it and jump; else pop.
    AndJump(u32),
    /// if top is truthy: keep it and jump; else pop.
    OrJump(u32),
    /// if top is not null: keep it and jump; else pop.
    NullishJump(u32),
    /// if top is null: keep it (as null) and jump.
    JumpIfNullKeep(u32),
    Call(u32),
    /// Call with args packed in one array: [callee, argsArray]
    CallSpread,
    /// [recv, args...] -> result; name is a const index.
    CallMethod(u32, u32),
    CallMethodSpread(u32),
    Return,
    Closure(u32),
    NewArray(u32),
    ArrayPush,
    ArraySpread,
    NewObject,
    /// [obj, v] -> [obj]  (object literal construction)
    InitProp(u32),
    ObjSpread,
    GetProp(u32),
    /// [obj, v] -> [v]
    SetProp(u32),
    GetIndex,
    /// [obj, idx, v] -> [v]
    SetIndex,
    Len,
    ToIter,
    Throw,
    /// Register a handler that jumps to the target on exception.
    TryBegin(u32),
    TryEnd,
    /// Push the in-flight exception (used at the start of catch blocks).
    Await,
    Concat(u32),
    /// Assert not null (`x!`)
    NonNull,
    /// Throw a TypeError with the const string message.
    Fail(u32),
    /// Object rest: [obj] -> [newObj] excluding keys listed (joined by NUL) in const.
    ObjRest(u32),
    /// Array rest: [arr] -> [arr.slice(n)]
    ArrRest(u32),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Const {
    Num(f64),
    Str(String),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct UpvalDesc {
    /// true: capture a cell of the enclosing function; false: re-capture an upvalue of it.
    pub from_cell: bool,
    pub index: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FuncProto {
    pub name: String,
    pub module: String,
    pub n_params: u32,
    pub has_rest: bool,
    pub n_slots: u32,
    pub n_cells: u32,
    pub is_async: bool,
    pub code: Vec<Op>,
    pub lines: Vec<u32>,
    pub consts: Vec<Const>,
    pub upvals: Vec<UpvalDesc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum ResultKind {
    /// Function returns a ToolResult: pass through.
    Passthrough,
    /// Returns string: text content.
    Text,
    /// Returns an object: structuredContent + JSON text.
    Structured,
    /// Returns other JSON-able value: JSON text.
    Json,
    Void,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub title: Option<String>,
    pub description: String,
    pub input_schema: Json,
    pub output_schema: Option<Json>,
    pub annotations: Json,
    pub global: u32,
    pub params: Vec<String>,
    pub result: ResultKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceDef {
    pub uri: String,
    pub is_template: bool,
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub mime_type: Option<String>,
    pub global: u32,
    pub params: Vec<String>,
    /// "string" | "content" | "contents" (ResourceContent[])
    pub result: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptArg {
    pub name: String,
    pub description: Option<String>,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptDef {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub arguments: Vec<PromptArg>,
    pub global: u32,
    /// "string" | "messages"
    pub result: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestDef {
    pub name: String,
    pub module: String,
    pub func: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalInfo {
    pub name: String,
    /// Native binding such as "std/fs.readFile" or "global.Math".
    pub native: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Program {
    pub funcs: Vec<FuncProto>,
    pub globals: Vec<GlobalInfo>,
    /// Module init functions in dependency order.
    pub inits: Vec<u32>,
    pub tools: Vec<ToolDef>,
    pub resources: Vec<ResourceDef>,
    pub prompts: Vec<PromptDef>,
    pub tests: Vec<TestDef>,
}
