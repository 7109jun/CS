use crate::ast::*;
use crate::bytecode::*;
use crate::checker::pattern_names;
use crate::diag::Diagnostic;
use crate::lexer::Span;
use crate::project::Loaded;
use serde_json::{json, Value as Json};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

// ======================================================================
// capture analysis
// ======================================================================

struct Analyzer {
    scopes: Vec<HashMap<String, (Span, usize)>>,
    level: usize,
    captured: HashSet<Span>,
}

impl Analyzer {
    fn is_module_scope(&self) -> bool {
        self.level == 0 && self.scopes.len() == 1
    }
    fn declare(&mut self, name: &str, span: Span) {
        if self.is_module_scope() {
            return;
        }
        let lvl = self.level;
        self.scopes.last_mut().unwrap().insert(name.to_string(), (span, lvl));
    }
    fn declare_pattern(&mut self, p: &Pattern) {
        let mut names = Vec::new();
        pattern_names(p, &mut names);
        for (n, sp) in names {
            self.declare(&n, sp);
        }
        self.pattern_defaults(p);
    }
    fn pattern_defaults(&mut self, p: &Pattern) {
        match p {
            Pattern::Ident(..) => {}
            Pattern::Object { props, .. } => {
                for pr in props {
                    if let Some(d) = &pr.default {
                        self.expr(d);
                    }
                    self.pattern_defaults(&pr.target);
                }
            }
            Pattern::Array { elems, .. } => {
                for e in elems.iter().flatten() {
                    if let Some(d) = &e.default {
                        self.expr(d);
                    }
                    self.pattern_defaults(&e.target);
                }
            }
        }
    }
    fn use_name(&mut self, name: &str) {
        for sc in self.scopes.iter().rev() {
            if let Some((sp, lvl)) = sc.get(name) {
                if *lvl < self.level {
                    self.captured.insert(*sp);
                }
                return;
            }
        }
    }
    fn func(&mut self, f: &FuncDecl) {
        self.level += 1;
        self.scopes.push(HashMap::new());
        for p in &f.params {
            self.declare_pattern(&p.pattern);
        }
        for p in &f.params {
            if let Some(d) = &p.default {
                self.expr(d);
            }
        }
        self.stmts(&f.body);
        self.scopes.pop();
        self.level -= 1;
    }
    fn stmts(&mut self, v: &[Stmt]) {
        for s in v {
            if let StmtKind::Func(f) = &s.kind {
                if let Some(n) = &f.name {
                    self.declare(n, f.span);
                }
            }
        }
        for s in v {
            self.stmt(s);
        }
    }
    fn block(&mut self, v: &[Stmt]) {
        self.scopes.push(HashMap::new());
        self.stmts(v);
        self.scopes.pop();
    }
    fn branch(&mut self, s: &Stmt) {
        self.scopes.push(HashMap::new());
        self.stmt(s);
        self.scopes.pop();
    }
    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Var { pattern, init, .. } => {
                if let Some(i) = init {
                    self.expr(i);
                }
                self.declare_pattern(pattern);
            }
            StmtKind::Expr(e) | StmtKind::Throw(e) => self.expr(e),
            StmtKind::Block(b) => self.block(b),
            StmtKind::If(c, a, b) => {
                self.expr(c);
                self.branch(a);
                if let Some(b) = b {
                    self.branch(b);
                }
            }
            StmtKind::While(c, b) => {
                self.expr(c);
                self.branch(b);
            }
            StmtKind::DoWhile(b, c) => {
                self.branch(b);
                self.expr(c);
            }
            StmtKind::For { init, cond, update, body } => {
                self.scopes.push(HashMap::new());
                if let Some(i) = init {
                    self.stmt(i);
                }
                if let Some(c) = cond {
                    self.expr(c);
                }
                if let Some(u) = update {
                    self.expr(u);
                }
                self.branch(body);
                self.scopes.pop();
            }
            StmtKind::ForOf { pattern, iter, body, .. } => {
                self.expr(iter);
                self.scopes.push(HashMap::new());
                self.declare_pattern(pattern);
                self.branch(body);
                self.scopes.pop();
            }
            StmtKind::Switch(d, cases) => {
                self.expr(d);
                self.scopes.push(HashMap::new());
                for c in cases {
                    if let Some(t) = &c.test {
                        self.expr(t);
                    }
                    self.stmts(&c.body);
                }
                self.scopes.pop();
            }
            StmtKind::Return(Some(e)) => self.expr(e),
            StmtKind::Try { block, catch, finally } => {
                self.block(block);
                if let Some((b, c)) = catch {
                    self.scopes.push(HashMap::new());
                    if let Some(p) = b {
                        self.declare_pattern(p);
                    }
                    self.stmts(c);
                    self.scopes.pop();
                }
                if let Some(f) = finally {
                    self.block(f);
                }
            }
            StmtKind::Func(f) => self.func(f),
            StmtKind::Test { body, .. } => {
                self.level += 1;
                self.scopes.push(HashMap::new());
                self.stmts(body);
                self.scopes.pop();
                self.level -= 1;
            }
            _ => {}
        }
    }
    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Ident(n) => self.use_name(n),
            ExprKind::Template(segs) => {
                for s in segs {
                    if let TemplateSeg::Expr(x) = s {
                        self.expr(x);
                    }
                }
            }
            ExprKind::Array(items) => {
                for i in items {
                    match i {
                        ArrElem::Item(x) | ArrElem::Spread(x) => self.expr(x),
                    }
                }
            }
            ExprKind::Object(props) => {
                for p in props {
                    match p {
                        ObjProp::KV(_, x) | ObjProp::Spread(x) => self.expr(x),
                    }
                }
            }
            ExprKind::Func(f) => self.func(f),
            ExprKind::Unary(_, x) | ExprKind::Await(x) | ExprKind::TypeOf(x) | ExprKind::As(x, _) | ExprKind::NonNull(x) | ExprKind::Chain(x) => self.expr(x),
            ExprKind::Binary(_, a, b) | ExprKind::Logical(_, a, b) => {
                self.expr(a);
                self.expr(b);
            }
            ExprKind::Assign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            ExprKind::Update { target, .. } => self.expr(target),
            ExprKind::Cond(a, b, c) => {
                self.expr(a);
                self.expr(b);
                self.expr(c);
            }
            ExprKind::Call { callee, args, .. } => {
                self.expr(callee);
                for a in args {
                    match a {
                        Arg::Item(x) | Arg::Spread(x) => self.expr(x),
                    }
                }
            }
            ExprKind::Member { obj, .. } => self.expr(obj),
            ExprKind::Index { obj, idx, .. } => {
                self.expr(obj);
                self.expr(idx);
            }
            _ => {}
        }
    }
}

// ======================================================================
// compiler
// ======================================================================

#[derive(Clone, Copy, PartialEq, Debug)]
enum Loc {
    Slot(u32),
    Cell(u32),
}

#[derive(Clone, Copy, Debug)]
enum VarRef {
    Local(u32),
    Cell(u32),
    Upval(u32),
    Global(u32),
    Ns(usize),
}

#[derive(Clone, Copy)]
enum TopRef {
    Global(u32),
    Ns(usize),
}

struct LoopCtx {
    breaks: Vec<usize>,
    continues: Vec<usize>,
    try_depth: usize,
    is_switch: bool,
}

#[derive(Clone)]
struct TryCtx {
    depth_before: u32,
    finally: Option<Rc<Vec<Stmt>>>,
}

struct FuncState {
    name: String,
    code: Vec<Op>,
    lines: Vec<u32>,
    consts: Vec<Const>,
    str_consts: HashMap<String, u32>,
    num_consts: HashMap<u64, u32>,
    scopes: Vec<(HashMap<String, Loc>, u32)>,
    next_slot: u32,
    max_slot: u32,
    n_cells: u32,
    n_params: u32,
    has_rest: bool,
    is_async: bool,
    upvals: Vec<UpvalDesc>,
    chain_stack: Vec<Vec<usize>>,
    loops: Vec<LoopCtx>,
    tries: Vec<TryCtx>,
    handler_depth: u32,
    is_init: bool,
}

impl FuncState {
    fn new(name: &str, is_async: bool, is_init: bool) -> FuncState {
        FuncState {
            name: name.to_string(),
            code: vec![],
            lines: vec![],
            consts: vec![],
            str_consts: HashMap::new(),
            num_consts: HashMap::new(),
            scopes: vec![(HashMap::new(), 0)],
            next_slot: 0,
            max_slot: 0,
            n_cells: 0,
            n_params: 0,
            has_rest: false,
            is_async,
            upvals: vec![],
            chain_stack: vec![],
            loops: vec![],
            tries: vec![],
            handler_depth: 0,
            is_init,
        }
    }
}

pub struct Compiler<'a> {
    loaded: &'a Loaded,
    prog: Program,
    exports: Vec<HashMap<String, u32>>,
    top: Vec<HashMap<String, TopRef>>,
    fstack: Vec<FuncState>,
    cur_mod: usize,
    captured: HashSet<Span>,
    diags: Vec<Diagnostic>,
    include_tests: bool,
    line: u32,
    global_names: HashMap<(usize, String), u32>,
}

pub fn compile_program(loaded: &Loaded, include_tests: bool) -> Result<Program, Vec<Diagnostic>> {
    let n = loaded.modules.len();
    let mut c = Compiler {
        loaded,
        prog: Program::default(),
        exports: vec![HashMap::new(); n],
        top: vec![HashMap::new(); n],
        fstack: vec![],
        cur_mod: 0,
        captured: HashSet::new(),
        diags: vec![],
        include_tests,
        line: 0,
        global_names: HashMap::new(),
    };
    for id in 0..n {
        c.compile_module(id);
    }
    if c.diags.is_empty() {
        Ok(c.prog)
    } else {
        Err(c.diags)
    }
}

impl<'a> Compiler<'a> {
    fn ice(&mut self, span: Span, msg: impl Into<String>) {
        let file = self.loaded.modules[self.cur_mod].display.clone();
        self.diags.push(Diagnostic::error(&file, span, format!("internal compiler error: {}", msg.into())));
    }

    fn new_global(&mut self, name: &str, native: Option<String>) -> u32 {
        let id = self.prog.globals.len() as u32;
        self.prog.globals.push(GlobalInfo { name: name.to_string(), native });
        id
    }

    fn value_export(&self, module: usize, name: &str) -> Option<u32> {
        let src = &self.loaded.modules[module];
        if src.module.is_header && !src.is_std {
            if let Some(imp) = src.impl_id {
                return self.exports[imp].get(name).copied();
            }
            return None;
        }
        self.exports[module].get(name).copied()
    }

    // ------------------------------------------------------------------
    // module-level
    // ------------------------------------------------------------------
    fn compile_module(&mut self, id: usize) {
        self.cur_mod = id;
        let loaded = self.loaded;
        let src = &loaded.modules[id];
        let module_path = src.display.clone();

        // declare natives for std headers
        if src.is_std {
            let prefix = if src.key == "std/prelude" { "global".to_string() } else { src.key.clone() };
            for s in &src.module.body {
                match &s.kind {
                    StmtKind::DeclareFunc { name, .. } | StmtKind::DeclareConst { name, .. } => {
                        let slot = self.new_global(name, Some(format!("{}.{}", prefix, name)));
                        self.top[id].insert(name.clone(), TopRef::Global(slot));
                        self.exports[id].insert(name.clone(), slot);
                    }
                    _ => {}
                }
            }
            return;
        }
        if src.module.is_header {
            // header: types only (implementation compiled with its own module); still process imports for nothing
            return;
        }

        // imports
        for s in &src.module.body {
            if let StmtKind::Import { specs, ns, from, type_only } = &s.kind {
                let Some(&dep) = src.deps.get(from) else { continue };
                if let Some(nsn) = ns {
                    self.top[id].insert(nsn.clone(), TopRef::Ns(dep));
                    continue;
                }
                if *type_only {
                    continue;
                }
                for sp in specs {
                    if let Some(slot) = self.value_export(dep, &sp.name) {
                        self.top[id].insert(sp.alias.clone(), TopRef::Global(slot));
                    }
                }
            }
        }
        // prelude names are visible through top[0]
        // top-level declarations get global slots
        let mut local_names: Vec<(String, u32, bool)> = Vec::new();
        for s in &src.module.body {
            match &s.kind {
                StmtKind::Func(f) => {
                    let n = f.name.clone().unwrap();
                    let slot = self.new_global(&format!("{}::{}", module_path, n), None);
                    self.top[id].insert(n.clone(), TopRef::Global(slot));
                    local_names.push((n, slot, f.exported));
                }
                StmtKind::Var { pattern, exported, .. } => {
                    let mut names = Vec::new();
                    pattern_names(pattern, &mut names);
                    for (n, _) in names {
                        let slot = self.new_global(&format!("{}::{}", module_path, n), None);
                        self.top[id].insert(n.clone(), TopRef::Global(slot));
                        local_names.push((n, slot, *exported));
                    }
                }
                _ => {}
            }
        }
        for (n, slot, exported) in &local_names {
            self.global_names.insert((id, n.clone()), *slot);
            if *exported {
                self.exports[id].insert(n.clone(), *slot);
            }
        }
        for s in &src.module.body {
            if let StmtKind::ExportNames(specs) = &s.kind {
                for sp in specs {
                    if let Some(TopRef::Global(slot)) = self.top[id].get(&sp.name).copied() {
                        self.exports[id].insert(sp.alias.clone(), slot);
                    }
                }
            }
        }

        // capture analysis
        let mut an = Analyzer { scopes: vec![HashMap::new()], level: 0, captured: HashSet::new() };
        an.stmts(&src.module.body);
        self.captured = an.captured;

        // module init function
        self.fstack.push(FuncState::new(&format!("<init {}>", module_path), true, true));
        // hoist top-level functions
        for s in &src.module.body {
            if let StmtKind::Func(f) = &s.kind {
                let idx = self.compile_function(f, f.name.clone().unwrap_or_default());
                self.emit(Op::Closure(idx));
                let slot = self.global_names[&(id, f.name.clone().unwrap())];
                self.emit(Op::SetGlobal(slot));
            }
        }
        let mut deferred: Vec<&Rc<FuncDecl>> = Vec::new();
        for s in &src.module.body {
            self.line = s.span.line;
            match &s.kind {
                StmtKind::Func(f) => {
                    if !f.decorators.is_empty() {
                        deferred.push(f);
                    }
                }
                StmtKind::Test { name, body } => {
                    if self.include_tests {
                        let idx = self.compile_body_function(body, &format!("test: {}", name), true);
                        self.prog.tests.push(TestDef { name: name.clone(), module: module_path.clone(), func: idx });
                    }
                }
                _ => self.stmt(s),
            }
        }
        self.emit(Op::Null);
        self.emit(Op::Return);
        let fs = self.fstack.pop().unwrap();
        let init_idx = self.finish_func(fs);
        self.prog.inits.push(init_idx);
        for f in deferred {
            self.register_decorated(id, f);
        }
    }

    fn register_decorated(&mut self, module: usize, f: &Rc<FuncDecl>) {
        let meta = f.meta.borrow().clone();
        let Some((kind, args)) = meta.decorator_opts.first().cloned() else { return };
        let slot = self.global_names[&(module, f.name.clone().unwrap())];
        let fname = f.name.clone().unwrap();
        let opts0 = match args.first() {
            Some(Json::String(s)) => json!({"description": s}),
            Some(o @ Json::Object(_)) => o.clone(),
            _ => json!({}),
        };
        let get_s = |o: &Json, k: &str| o.get(k).and_then(|v| v.as_str()).map(|s| s.to_string());
        match kind.as_str() {
            "tool" => {
                let name = get_s(&opts0, "name").unwrap_or(fname);
                let mut ann = serde_json::Map::new();
                if let Some(t) = get_s(&opts0, "title") {
                    ann.insert("title".into(), json!(t));
                }
                for (k, a) in [("readOnly", "readOnlyHint"), ("destructive", "destructiveHint"), ("idempotent", "idempotentHint"), ("openWorld", "openWorldHint")] {
                    if let Some(b) = opts0.get(k).and_then(|v| v.as_bool()) {
                        ann.insert(a.into(), json!(b));
                    }
                }
                let result = match meta.result_kind.as_str() {
                    "passthrough" => ResultKind::Passthrough,
                    "text" => ResultKind::Text,
                    "structured" => ResultKind::Structured,
                    "void" => ResultKind::Void,
                    _ => ResultKind::Json,
                };
                self.prog.tools.push(ToolDef {
                    name,
                    title: get_s(&opts0, "title"),
                    description: get_s(&opts0, "description").unwrap_or_default(),
                    input_schema: meta.input_schema.clone().unwrap_or(json!({"type": "object"})),
                    output_schema: meta.output_schema.clone(),
                    annotations: Json::Object(ann),
                    global: slot,
                    params: meta.param_names.clone(),
                    result,
                });
            }
            "resource" => {
                let uri = args.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
                let opts = args.get(1).cloned().unwrap_or(json!({}));
                self.prog.resources.push(ResourceDef {
                    is_template: uri.contains('{'),
                    uri,
                    name: get_s(&opts, "name").unwrap_or(fname),
                    title: get_s(&opts, "title"),
                    description: get_s(&opts, "description"),
                    mime_type: get_s(&opts, "mimeType"),
                    global: slot,
                    params: meta.param_names.clone(),
                    result: meta.result_kind.clone(),
                });
            }
            "prompt" => {
                let descs = opts0.get("params").and_then(|p| p.as_object()).cloned().unwrap_or_default();
                let arguments = meta
                    .param_names
                    .iter()
                    .zip(meta.param_required.iter())
                    .map(|(n, r)| PromptArg { name: n.clone(), description: descs.get(n).and_then(|d| d.as_str()).map(|s| s.to_string()), required: *r })
                    .collect();
                self.prog.prompts.push(PromptDef {
                    name: get_s(&opts0, "name").unwrap_or(fname),
                    title: get_s(&opts0, "title"),
                    description: get_s(&opts0, "description"),
                    arguments,
                    global: slot,
                    result: meta.result_kind.clone(),
                });
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------------
    // emit helpers
    // ------------------------------------------------------------------
    fn fs(&mut self) -> &mut FuncState {
        self.fstack.last_mut().unwrap()
    }

    fn emit(&mut self, op: Op) -> usize {
        let line = self.line;
        let fs = self.fs();
        fs.code.push(op);
        fs.lines.push(line);
        fs.code.len() - 1
    }

    fn here(&mut self) -> usize {
        self.fs().code.len()
    }

    fn patch(&mut self, at: usize, target: usize) {
        let t = target as u32;
        let fs = self.fs();
        fs.code[at] = match fs.code[at] {
            Op::Jump(_) => Op::Jump(t),
            Op::JumpIfFalse(_) => Op::JumpIfFalse(t),
            Op::JumpIfTrue(_) => Op::JumpIfTrue(t),
            Op::AndJump(_) => Op::AndJump(t),
            Op::OrJump(_) => Op::OrJump(t),
            Op::NullishJump(_) => Op::NullishJump(t),
            Op::JumpIfNullKeep(_) => Op::JumpIfNullKeep(t),
            Op::TryBegin(_) => Op::TryBegin(t),
            other => other,
        };
    }

    fn patch_here(&mut self, at: usize) {
        let h = self.here();
        self.patch(at, h);
    }

    fn const_str(&mut self, s: &str) -> u32 {
        let fs = self.fs();
        if let Some(&i) = fs.str_consts.get(s) {
            return i;
        }
        fs.consts.push(Const::Str(s.to_string()));
        let i = fs.consts.len() as u32 - 1;
        fs.str_consts.insert(s.to_string(), i);
        i
    }

    fn const_num(&mut self, n: f64) -> u32 {
        let fs = self.fs();
        let key = n.to_bits();
        if let Some(&i) = fs.num_consts.get(&key) {
            return i;
        }
        fs.consts.push(Const::Num(n));
        let i = fs.consts.len() as u32 - 1;
        fs.num_consts.insert(key, i);
        i
    }

    fn alloc_slot(&mut self) -> u32 {
        let fs = self.fs();
        let s = fs.next_slot;
        fs.next_slot += 1;
        if fs.next_slot > fs.max_slot {
            fs.max_slot = fs.next_slot;
        }
        s
    }

    fn alloc_cell(&mut self) -> u32 {
        let fs = self.fs();
        fs.n_cells += 1;
        fs.n_cells - 1
    }

    fn push_scope(&mut self) {
        let fs = self.fs();
        let ns = fs.next_slot;
        fs.scopes.push((HashMap::new(), ns));
    }

    fn pop_scope(&mut self) {
        let fs = self.fs();
        let (_, ns) = fs.scopes.pop().unwrap();
        fs.next_slot = ns;
    }

    fn at_module_level(&self) -> bool {
        let fs = self.fstack.last().unwrap();
        fs.is_init && self.fstack.len() == 1 && fs.scopes.len() == 1
    }

    fn finish_func(&mut self, fs: FuncState) -> u32 {
        let proto = FuncProto {
            name: fs.name,
            module: self.loaded.modules[self.cur_mod].display.clone(),
            n_params: fs.n_params,
            has_rest: fs.has_rest,
            n_slots: fs.max_slot.max(fs.n_params),
            n_cells: fs.n_cells,
            is_async: fs.is_async,
            code: fs.code,
            lines: fs.lines,
            consts: fs.consts,
            upvals: fs.upvals,
        };
        self.prog.funcs.push(proto);
        self.prog.funcs.len() as u32 - 1
    }

    // ------------------------------------------------------------------
    // variable resolution
    // ------------------------------------------------------------------
    fn resolve_in(&mut self, lvl: usize, name: &str) -> Option<VarRef> {
        for (sc, _) in self.fstack[lvl].scopes.iter().rev() {
            if let Some(loc) = sc.get(name) {
                return Some(match loc {
                    Loc::Slot(s) => VarRef::Local(*s),
                    Loc::Cell(c) => VarRef::Cell(*c),
                });
            }
        }
        if lvl == 0 {
            return None;
        }
        let outer = self.resolve_in(lvl - 1, name)?;
        let desc = match outer {
            VarRef::Cell(c) => UpvalDesc { from_cell: true, index: c },
            VarRef::Upval(u) => UpvalDesc { from_cell: false, index: u },
            VarRef::Local(_) => {
                let sp = Span::default();
                self.ice(sp, format!("variable '{}' is captured by a closure but was not boxed", name));
                return None;
            }
            _ => return None,
        };
        let fs = &mut self.fstack[lvl];
        if let Some(i) = fs.upvals.iter().position(|u| u.from_cell == desc.from_cell && u.index == desc.index) {
            return Some(VarRef::Upval(i as u32));
        }
        fs.upvals.push(desc);
        Some(VarRef::Upval(fs.upvals.len() as u32 - 1))
    }

    fn resolve(&mut self, name: &str) -> Option<VarRef> {
        let top = self.fstack.len() - 1;
        if let Some(v) = self.resolve_in(top, name) {
            return Some(v);
        }
        match self.top[self.cur_mod].get(name) {
            Some(TopRef::Global(s)) => return Some(VarRef::Global(*s)),
            Some(TopRef::Ns(m)) => return Some(VarRef::Ns(*m)),
            None => {}
        }
        match self.top[0].get(name) {
            Some(TopRef::Global(s)) => Some(VarRef::Global(*s)),
            _ => None,
        }
    }

    fn emit_get(&mut self, name: &str, span: Span) {
        match self.resolve(name) {
            Some(VarRef::Local(s)) => {
                self.emit(Op::GetLocal(s));
            }
            Some(VarRef::Cell(c)) => {
                self.emit(Op::GetCell(c));
            }
            Some(VarRef::Upval(u)) => {
                self.emit(Op::GetUpval(u));
            }
            Some(VarRef::Global(g)) => {
                self.emit(Op::GetGlobal(g));
            }
            Some(VarRef::Ns(_)) => {
                self.ice(span, format!("namespace '{}' used as a value", name));
                self.emit(Op::Null);
            }
            None => {
                self.ice(span, format!("unresolved name '{}'", name));
                self.emit(Op::Null);
            }
        }
    }

    /// Pops the top of stack into the variable.
    fn emit_set(&mut self, name: &str, span: Span) {
        match self.resolve(name) {
            Some(VarRef::Local(s)) => {
                self.emit(Op::SetLocal(s));
            }
            Some(VarRef::Cell(c)) => {
                self.emit(Op::SetCell(c));
            }
            Some(VarRef::Upval(u)) => {
                self.emit(Op::SetUpval(u));
            }
            Some(VarRef::Global(g)) => {
                self.emit(Op::SetGlobal(g));
            }
            _ => {
                self.ice(span, format!("cannot assign to '{}'", name));
                self.emit(Op::Pop);
            }
        }
    }

    /// Declare a new local (or module global) initialized from the value on top of the stack.
    fn declare_from_stack(&mut self, name: &str, span: Span) {
        if self.at_module_level() {
            let slot = self.global_names.get(&(self.cur_mod, name.to_string())).copied();
            match slot {
                Some(s) => {
                    self.emit(Op::SetGlobal(s));
                }
                None => {
                    self.ice(span, format!("unknown top-level name '{}'", name));
                    self.emit(Op::Pop);
                }
            }
            return;
        }
        if self.captured.contains(&span) {
            let c = self.alloc_cell();
            self.emit(Op::NewCell(c));
            self.fs().scopes.last_mut().unwrap().0.insert(name.to_string(), Loc::Cell(c));
        } else {
            let s = self.alloc_slot();
            self.emit(Op::SetLocal(s));
            self.fs().scopes.last_mut().unwrap().0.insert(name.to_string(), Loc::Slot(s));
        }
    }

    // ------------------------------------------------------------------
    // functions
    // ------------------------------------------------------------------
    fn compile_body_function(&mut self, body: &[Stmt], name: &str, is_async: bool) -> u32 {
        self.fstack.push(FuncState::new(name, is_async, false));
        self.hoist_funcs(body);
        for s in body {
            self.stmt(s);
        }
        self.emit(Op::Null);
        self.emit(Op::Return);
        let fs = self.fstack.pop().unwrap();
        self.finish_func(fs)
    }

    fn compile_function(&mut self, f: &FuncDecl, name: String) -> u32 {
        let saved_line = self.line;
        self.line = f.span.line;
        self.fstack.push(FuncState::new(&name, f.is_async, false));
        {
            let fs = self.fs();
            fs.n_params = f.params.len() as u32;
            fs.has_rest = f.params.last().map_or(false, |p| p.rest);
            fs.next_slot = fs.n_params;
            fs.max_slot = fs.n_params;
        }
        // bind simple parameters
        for (i, p) in f.params.iter().enumerate() {
            if let Pattern::Ident(n, sp) = &p.pattern {
                if self.captured.contains(sp) {
                    let c = self.alloc_cell();
                    self.emit(Op::GetLocal(i as u32));
                    self.emit(Op::NewCell(c));
                    self.fs().scopes.last_mut().unwrap().0.insert(n.clone(), Loc::Cell(c));
                } else {
                    self.fs().scopes.last_mut().unwrap().0.insert(n.clone(), Loc::Slot(i as u32));
                }
            }
        }
        // defaults, then destructuring (in order)
        for (i, p) in f.params.iter().enumerate() {
            if let Some(d) = &p.default {
                self.emit(Op::GetLocal(i as u32));
                self.emit(Op::Null);
                self.emit(Op::Eq);
                let j = self.emit(Op::JumpIfFalse(0));
                self.expr(d);
                self.emit(Op::SetLocal(i as u32));
                self.patch_here(j);
                if let Pattern::Ident(n, _) = &p.pattern {
                    // keep cell in sync
                    if let Some(Loc::Cell(c)) = self.fs().scopes[0].0.get(n).copied() {
                        self.emit(Op::GetLocal(i as u32));
                        self.emit(Op::SetCell(c));
                    }
                }
            }
            if !matches!(p.pattern, Pattern::Ident(..)) {
                self.emit(Op::GetLocal(i as u32));
                self.bind_pattern(&p.pattern);
            }
        }
        self.hoist_funcs(&f.body);
        for s in &f.body {
            self.stmt(s);
        }
        self.emit(Op::Null);
        self.emit(Op::Return);
        let fs = self.fstack.pop().unwrap();
        let idx = self.finish_func(fs);
        self.line = saved_line;
        idx
    }

    fn hoist_funcs(&mut self, stmts: &[Stmt]) {
        let funcs: Vec<&Rc<FuncDecl>> = stmts
            .iter()
            .filter_map(|s| match &s.kind {
                StmtKind::Func(f) => Some(f),
                _ => None,
            })
            .collect();
        if funcs.is_empty() || self.at_module_level() {
            return;
        }
        // declare all names first
        for f in &funcs {
            let n = f.name.clone().unwrap();
            if self.captured.contains(&f.span) {
                let c = self.alloc_cell();
                self.emit(Op::Null);
                self.emit(Op::NewCell(c));
                self.fs().scopes.last_mut().unwrap().0.insert(n, Loc::Cell(c));
            } else {
                let s = self.alloc_slot();
                self.fs().scopes.last_mut().unwrap().0.insert(n, Loc::Slot(s));
            }
        }
        for f in funcs {
            let n = f.name.clone().unwrap();
            let idx = self.compile_function(f, n.clone());
            self.emit(Op::Closure(idx));
            self.emit_set(&n, f.span);
        }
    }

    // ------------------------------------------------------------------
    // patterns
    // ------------------------------------------------------------------
    /// Value on top of the stack is consumed and bound to the pattern's variables.
    fn bind_pattern(&mut self, p: &Pattern) {
        match p {
            Pattern::Ident(n, sp) => self.declare_from_stack(n, *sp),
            Pattern::Object { props, rest, span } => {
                let tmp = self.alloc_slot();
                self.emit(Op::SetLocal(tmp));
                let mut keys = Vec::new();
                for pr in props {
                    keys.push(pr.key.clone());
                    self.emit(Op::GetLocal(tmp));
                    let k = self.const_str(&pr.key);
                    self.emit(Op::GetProp(k));
                    if let Some(d) = &pr.default {
                        let j = self.emit(Op::NullishJump(0));
                        self.expr(d);
                        self.patch_here(j);
                    }
                    self.bind_pattern(&pr.target);
                }
                if let Some(r) = rest {
                    self.emit(Op::GetLocal(tmp));
                    let k = self.const_str(&keys.join("\u{0}"));
                    self.emit(Op::ObjRest(k));
                    self.declare_from_stack(r, *span);
                }
            }
            Pattern::Array { elems, rest, span } => {
                let tmp = self.alloc_slot();
                self.emit(Op::SetLocal(tmp));
                for (i, el) in elems.iter().enumerate() {
                    let Some(el) = el else { continue };
                    self.emit(Op::GetLocal(tmp));
                    let k = self.const_num(i as f64);
                    self.emit(Op::Const(k));
                    self.emit(Op::GetIndex);
                    if let Some(d) = &el.default {
                        let j = self.emit(Op::NullishJump(0));
                        self.expr(d);
                        self.patch_here(j);
                    }
                    self.bind_pattern(&el.target);
                }
                if let Some(r) = rest {
                    self.emit(Op::GetLocal(tmp));
                    self.emit(Op::ArrRest(elems.len() as u32));
                    self.declare_from_stack(r, *span);
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // statements
    // ------------------------------------------------------------------
    fn stmts_scoped(&mut self, v: &[Stmt]) {
        self.push_scope();
        self.hoist_funcs(v);
        for s in v {
            self.stmt(s);
        }
        self.pop_scope();
    }

    fn branch(&mut self, s: &Stmt) {
        self.push_scope();
        self.stmt(s);
        self.pop_scope();
    }

    fn stmt(&mut self, s: &Stmt) {
        self.line = s.span.line;
        match &s.kind {
            StmtKind::Var { pattern, init, .. } => {
                match init {
                    Some(e) => self.expr(e),
                    None => {
                        self.emit(Op::Null);
                    }
                }
                self.bind_pattern(pattern);
            }
            StmtKind::Expr(e) => self.expr_discard(e),
            StmtKind::Block(b) => self.stmts_scoped(b),
            StmtKind::If(c, a, b) => {
                self.expr(c);
                let jf = self.emit(Op::JumpIfFalse(0));
                self.branch(a);
                if let Some(b) = b {
                    let je = self.emit(Op::Jump(0));
                    self.patch_here(jf);
                    self.branch(b);
                    self.patch_here(je);
                } else {
                    self.patch_here(jf);
                }
            }
            StmtKind::While(c, body) => {
                let start = self.here();
                self.expr(c);
                let jf = self.emit(Op::JumpIfFalse(0));
                self.push_loop(false);
                self.branch(body);
                let lp = self.pop_loop();
                for j in lp.continues {
                    self.patch(j, start);
                }
                self.emit(Op::Jump(start as u32));
                self.patch_here(jf);
                for j in lp.breaks {
                    self.patch_here(j);
                }
            }
            StmtKind::DoWhile(body, c) => {
                let start = self.here();
                self.push_loop(false);
                self.branch(body);
                let lp = self.pop_loop();
                for j in lp.continues {
                    self.patch_here(j);
                }
                self.expr(c);
                self.emit(Op::JumpIfTrue(start as u32));
                for j in lp.breaks {
                    self.patch_here(j);
                }
            }
            StmtKind::For { init, cond, update, body } => {
                self.push_scope();
                if let Some(i) = init {
                    self.stmt(i);
                }
                let start = self.here();
                let jf = cond.as_ref().map(|c| {
                    self.expr(c);
                    self.emit(Op::JumpIfFalse(0))
                });
                self.push_loop(false);
                self.branch(body);
                let lp = self.pop_loop();
                for j in lp.continues {
                    self.patch_here(j);
                }
                // fresh per-iteration copies of captured loop variables
                let cells: Vec<u32> = self.fstack.last().unwrap().scopes.last().unwrap().0.values().filter_map(|l| if let Loc::Cell(c) = l { Some(*c) } else { None }).collect();
                for c in cells {
                    self.emit(Op::RefreshCell(c));
                }
                if let Some(u) = update {
                    self.expr_discard(u);
                }
                self.emit(Op::Jump(start as u32));
                if let Some(j) = jf {
                    self.patch_here(j);
                }
                for j in lp.breaks {
                    self.patch_here(j);
                }
                self.pop_scope();
            }
            StmtKind::ForOf { pattern, iter, body, .. } => {
                self.push_scope();
                let it = self.alloc_slot();
                let idx = self.alloc_slot();
                self.expr(iter);
                self.emit(Op::ToIter);
                self.emit(Op::SetLocal(it));
                let z = self.const_num(0.0);
                self.emit(Op::Const(z));
                self.emit(Op::SetLocal(idx));
                let start = self.here();
                self.emit(Op::GetLocal(idx));
                self.emit(Op::GetLocal(it));
                self.emit(Op::Len);
                self.emit(Op::Lt);
                let jf = self.emit(Op::JumpIfFalse(0));
                self.push_scope();
                self.emit(Op::GetLocal(it));
                self.emit(Op::GetLocal(idx));
                self.emit(Op::GetIndex);
                self.bind_pattern(pattern);
                self.push_loop(false);
                self.branch(body);
                let lp = self.pop_loop();
                self.pop_scope();
                for j in lp.continues {
                    self.patch_here(j);
                }
                self.emit(Op::GetLocal(idx));
                let one = self.const_num(1.0);
                self.emit(Op::Const(one));
                self.emit(Op::Add);
                self.emit(Op::SetLocal(idx));
                self.emit(Op::Jump(start as u32));
                self.patch_here(jf);
                for j in lp.breaks {
                    self.patch_here(j);
                }
                self.pop_scope();
            }
            StmtKind::Switch(d, cases) => self.switch_stmt(d, cases),
            StmtKind::Return(v) => {
                match v {
                    Some(e) => self.expr(e),
                    None => {
                        self.emit(Op::Null);
                    }
                }
                self.unwind_tries(0, true);
                self.emit(Op::Return);
            }
            StmtKind::Break => {
                let idx = self.fstack.last().unwrap().loops.len().saturating_sub(1);
                if self.fstack.last().unwrap().loops.is_empty() {
                    self.ice(s.span, "break outside loop");
                    return;
                }
                let depth = self.fstack.last().unwrap().loops[idx].try_depth;
                self.unwind_tries(depth, false);
                let j = self.emit(Op::Jump(0));
                self.fs().loops[idx].breaks.push(j);
            }
            StmtKind::Continue => {
                let loops = &self.fstack.last().unwrap().loops;
                let Some(idx) = loops.iter().rposition(|l| !l.is_switch) else {
                    self.ice(s.span, "continue outside loop");
                    return;
                };
                let depth = loops[idx].try_depth;
                self.unwind_tries(depth, false);
                let j = self.emit(Op::Jump(0));
                self.fs().loops[idx].continues.push(j);
            }
            StmtKind::Throw(e) => {
                self.expr(e);
                self.emit(Op::Throw);
            }
            StmtKind::Try { block, catch, finally } => self.try_stmt(block, catch.as_ref(), finally.as_ref()),
            StmtKind::Func(_) => { /* hoisted */ }
            _ => {}
        }
    }

    fn push_loop(&mut self, is_switch: bool) {
        let fs = self.fs();
        let td = fs.tries.len();
        fs.loops.push(LoopCtx { breaks: vec![], continues: vec![], try_depth: td, is_switch });
    }

    fn pop_loop(&mut self) -> LoopCtx {
        self.fs().loops.pop().unwrap()
    }

    /// Emit the code needed to leave all try contexts above `down_to` (handler removal + inline finally).
    fn unwind_tries(&mut self, down_to: usize, _is_return: bool) {
        let saved_tries = self.fstack.last().unwrap().tries.clone();
        let saved_depth = self.fstack.last().unwrap().handler_depth;
        let n = saved_tries.len();
        for i in (down_to..n).rev() {
            let ctx = saved_tries[i].clone();
            let cur = self.fstack.last().unwrap().handler_depth;
            for _ in 0..(cur - ctx.depth_before) {
                self.emit(Op::TryEnd);
            }
            self.fs().handler_depth = ctx.depth_before;
            self.fs().tries.truncate(i);
            if let Some(f) = ctx.finally {
                self.stmts_scoped(&f);
            }
        }
        let fs = self.fs();
        fs.tries = saved_tries;
        fs.handler_depth = saved_depth;
    }

    fn try_stmt(&mut self, block: &[Stmt], catch: Option<&(Option<Pattern>, Vec<Stmt>)>, finally: Option<&Vec<Stmt>>) {
        let d0 = self.fstack.last().unwrap().handler_depth;
        let fin: Option<Rc<Vec<Stmt>>> = finally.map(|f| Rc::new(f.clone()));
        let mut l_fin_exc = None;
        if fin.is_some() {
            l_fin_exc = Some(self.emit(Op::TryBegin(0)));
            self.fs().handler_depth += 1;
            self.fs().tries.push(TryCtx { depth_before: d0, finally: fin.clone() });
        }
        if let Some((binding, cbody)) = catch {
            let d1 = self.fstack.last().unwrap().handler_depth;
            let l_catch = self.emit(Op::TryBegin(0));
            self.fs().handler_depth += 1;
            self.fs().tries.push(TryCtx { depth_before: d1, finally: None });
            self.stmts_scoped(block);
            self.fs().tries.pop();
            self.emit(Op::TryEnd);
            self.fs().handler_depth -= 1;
            let j_after = self.emit(Op::Jump(0));
            self.patch_here(l_catch);
            self.push_scope();
            match binding {
                Some(p) => self.bind_pattern(p),
                None => {
                    self.emit(Op::Pop);
                }
            }
            self.hoist_funcs(cbody);
            for s in cbody {
                self.stmt(s);
            }
            self.pop_scope();
            self.patch_here(j_after);
        } else {
            self.stmts_scoped(block);
        }
        if let Some(f) = &fin {
            self.fs().tries.pop();
            self.emit(Op::TryEnd);
            self.fs().handler_depth -= 1;
            self.stmts_scoped(f);
            let j_end = self.emit(Op::Jump(0));
            self.patch_here(l_fin_exc.unwrap());
            self.push_scope();
            let tmp = self.alloc_slot();
            self.emit(Op::SetLocal(tmp));
            self.stmts_scoped(f);
            self.emit(Op::GetLocal(tmp));
            self.emit(Op::Throw);
            self.pop_scope();
            self.patch_here(j_end);
        }
    }

    fn switch_stmt(&mut self, d: &Expr, cases: &[Case]) {
        self.push_scope();
        let tmp = self.alloc_slot();
        self.expr(d);
        self.emit(Op::SetLocal(tmp));
        let mut body_jumps: Vec<Option<usize>> = Vec::new();
        for c in cases {
            if let Some(t) = &c.test {
                self.emit(Op::GetLocal(tmp));
                self.expr(t);
                self.emit(Op::Eq);
                body_jumps.push(Some(self.emit(Op::JumpIfTrue(0))));
            } else {
                body_jumps.push(None);
            }
        }
        let j_default = self.emit(Op::Jump(0));
        self.push_loop(true);
        let mut default_target = None;
        for (i, c) in cases.iter().enumerate() {
            match body_jumps[i] {
                Some(j) => self.patch_here(j),
                None => default_target = Some(self.here()),
            }
            self.push_scope();
            self.hoist_funcs(&c.body);
            for s in &c.body {
                self.stmt(s);
            }
            self.pop_scope();
        }
        let lp = self.pop_loop();
        let end = self.here();
        self.patch(j_default, default_target.unwrap_or(end));
        for j in lp.breaks {
            self.patch(j, end);
        }
        self.pop_scope();
    }

    // ------------------------------------------------------------------
    // expressions
    // ------------------------------------------------------------------
    fn expr_discard(&mut self, e: &Expr) {
        self.line = e.span.line;
        match &e.kind {
            ExprKind::Assign { op: AssignOp::Assign, target, value } => {
                if let ExprKind::Ident(n) = &target.kind {
                    self.expr(value);
                    self.emit_set(n, e.span);
                    return;
                }
                self.expr(e);
                self.emit(Op::Pop);
            }
            ExprKind::Update { inc, target, .. } => {
                if let ExprKind::Ident(n) = &target.kind {
                    self.emit_get(n, e.span);
                    let one = self.const_num(1.0);
                    self.emit(Op::Const(one));
                    self.emit(if *inc { Op::Add } else { Op::Sub });
                    self.emit_set(n, e.span);
                    return;
                }
                self.expr(e);
                self.emit(Op::Pop);
            }
            _ => {
                self.expr(e);
                self.emit(Op::Pop);
            }
        }
    }

    fn binop(op: BinOp) -> Op {
        match op {
            BinOp::Add => Op::Add,
            BinOp::Sub => Op::Sub,
            BinOp::Mul => Op::Mul,
            BinOp::Div => Op::Div,
            BinOp::Mod => Op::Mod,
            BinOp::Pow => Op::Pow,
            BinOp::Eq => Op::Eq,
            BinOp::Ne => Op::Ne,
            BinOp::Lt => Op::Lt,
            BinOp::Le => Op::Le,
            BinOp::Gt => Op::Gt,
            BinOp::Ge => Op::Ge,
            BinOp::BitAnd => Op::BitAnd,
            BinOp::BitOr => Op::BitOr,
            BinOp::BitXor => Op::BitXor,
            BinOp::Shl => Op::Shl,
            BinOp::Shr => Op::Shr,
        }
    }

    fn expr(&mut self, e: &Expr) {
        self.line = e.span.line;
        match &e.kind {
            ExprKind::Num(n) => {
                let k = self.const_num(*n);
                self.emit(Op::Const(k));
            }
            ExprKind::Str(s) => {
                let k = self.const_str(s);
                self.emit(Op::Const(k));
            }
            ExprKind::Bool(b) => {
                self.emit(if *b { Op::True } else { Op::False });
            }
            ExprKind::Null => {
                self.emit(Op::Null);
            }
            ExprKind::Template(segs) => {
                if segs.is_empty() {
                    let k = self.const_str("");
                    self.emit(Op::Const(k));
                    return;
                }
                for s in segs {
                    match s {
                        TemplateSeg::Str(t) => {
                            let k = self.const_str(t);
                            self.emit(Op::Const(k));
                        }
                        TemplateSeg::Expr(x) => self.expr(x),
                    }
                }
                self.emit(Op::Concat(segs.len() as u32));
            }
            ExprKind::Ident(n) => self.emit_get(n, e.span),
            ExprKind::Array(items) => {
                let simple = items.iter().all(|i| matches!(i, ArrElem::Item(_)));
                if simple {
                    for i in items {
                        if let ArrElem::Item(x) = i {
                            self.expr(x);
                        }
                    }
                    self.emit(Op::NewArray(items.len() as u32));
                } else {
                    self.emit(Op::NewArray(0));
                    for i in items {
                        match i {
                            ArrElem::Item(x) => {
                                self.expr(x);
                                self.emit(Op::ArrayPush);
                            }
                            ArrElem::Spread(x) => {
                                self.expr(x);
                                self.emit(Op::ArraySpread);
                            }
                        }
                    }
                }
            }
            ExprKind::Object(props) => {
                self.emit(Op::NewObject);
                for p in props {
                    match p {
                        ObjProp::KV(k, v) => {
                            self.expr(v);
                            let kc = self.const_str(k);
                            self.emit(Op::InitProp(kc));
                        }
                        ObjProp::Spread(x) => {
                            self.expr(x);
                            self.emit(Op::ObjSpread);
                        }
                    }
                }
            }
            ExprKind::Func(f) => {
                let name = f.name.clone().unwrap_or_else(|| "<anonymous>".into());
                let idx = self.compile_function(f, name);
                self.emit(Op::Closure(idx));
            }
            ExprKind::Unary(op, x) => {
                self.expr(x);
                self.emit(match op {
                    UnOp::Neg => Op::Neg,
                    UnOp::Plus => Op::Plus,
                    UnOp::Not => Op::Not,
                    UnOp::BitNot => Op::BitNot,
                });
            }
            ExprKind::Binary(op, l, r) => {
                self.expr(l);
                self.expr(r);
                self.line = e.span.line;
                self.emit(Self::binop(*op));
            }
            ExprKind::Logical(op, l, r) => {
                self.expr(l);
                let j = self.emit(match op {
                    LogOp::And => Op::AndJump(0),
                    LogOp::Or => Op::OrJump(0),
                    LogOp::Nullish => Op::NullishJump(0),
                });
                self.expr(r);
                self.patch_here(j);
            }
            ExprKind::Cond(c, a, b) => {
                self.expr(c);
                let jf = self.emit(Op::JumpIfFalse(0));
                self.expr(a);
                let je = self.emit(Op::Jump(0));
                self.patch_here(jf);
                self.expr(b);
                self.patch_here(je);
            }
            ExprKind::Assign { op, target, value } => self.assign(*op, target, value, e.span),
            ExprKind::Update { inc, prefix, target } => self.update(*inc, *prefix, target, e.span),
            ExprKind::Call { callee, args, optional, .. } => self.call(callee, args, *optional, e.span),
            ExprKind::Member { obj, name, optional } => {
                if let Some(slot) = self.namespace_member(obj, name) {
                    self.emit(Op::GetGlobal(slot));
                    return;
                }
                self.expr(obj);
                if *optional {
                    self.chain_jump();
                }
                let k = self.const_str(name);
                self.emit(Op::GetProp(k));
            }
            ExprKind::Index { obj, idx, optional } => {
                self.expr(obj);
                if *optional {
                    self.chain_jump();
                }
                self.expr(idx);
                self.emit(Op::GetIndex);
            }
            ExprKind::Chain(inner) => {
                self.fs().chain_stack.push(vec![]);
                self.expr(inner);
                let jumps = self.fs().chain_stack.pop().unwrap();
                for j in jumps {
                    self.patch_here(j);
                }
            }
            ExprKind::Await(x) => {
                self.expr(x);
                self.emit(Op::Await);
            }
            ExprKind::TypeOf(x) => {
                self.expr(x);
                self.emit(Op::TypeOf);
            }
            ExprKind::As(x, _) => self.expr(x),
            ExprKind::NonNull(x) => {
                self.expr(x);
                self.emit(Op::NonNull);
            }
        }
    }

    fn chain_jump(&mut self) {
        let j = self.emit(Op::JumpIfNullKeep(0));
        match self.fs().chain_stack.last_mut() {
            Some(v) => v.push(j),
            None => {
                self.ice(Span::default(), "optional chain outside Chain node");
            }
        }
    }

    fn namespace_member(&mut self, obj: &Expr, name: &str) -> Option<u32> {
        if let ExprKind::Ident(n) = &obj.kind {
            if let Some(VarRef::Ns(m)) = self.resolve(n) {
                return match self.value_export(m, name) {
                    Some(s) => Some(s),
                    None => {
                        self.ice(obj.span, format!("module member '{}.{}' not found", n, name));
                        Some(0)
                    }
                };
            }
        }
        None
    }

    fn call(&mut self, callee: &Expr, args: &[Arg], optional: bool, span: Span) {
        let has_spread = args.iter().any(|a| matches!(a, Arg::Spread(_)));
        // method call
        if let ExprKind::Member { obj, name, optional: mopt } = &callee.kind {
            if self.namespace_member(obj, name).is_none() {
                self.expr(obj);
                if *mopt {
                    self.chain_jump();
                }
                let k = self.const_str(name);
                let mut opt_jumps: Option<usize> = None;
                if optional {
                    // a.b?.(): [recv] -> test the method value; on null the result is null
                    self.emit(Op::Dup);
                    self.emit(Op::GetProp(k));
                    opt_jumps = Some(self.emit(Op::JumpIfNullKeep(0)));
                    self.emit(Op::Pop);
                }
                if has_spread {
                    self.build_args_array(args);
                    self.emit(Op::CallMethodSpread(k));
                } else {
                    for a in args {
                        if let Arg::Item(x) = a {
                            self.expr(x);
                        }
                    }
                    self.emit(Op::CallMethod(k, args.len() as u32));
                }
                if let Some(j) = opt_jumps {
                    let je = self.emit(Op::Jump(0));
                    self.patch_here(j);
                    self.emit(Op::Swap);
                    self.emit(Op::Pop);
                    self.patch_here(je);
                }
                return;
            }
        }
        self.expr(callee);
        if optional {
            self.chain_jump();
        }
        if has_spread {
            self.build_args_array(args);
            self.emit(Op::CallSpread);
        } else {
            for a in args {
                if let Arg::Item(x) = a {
                    self.expr(x);
                }
            }
            self.emit(Op::Call(args.len() as u32));
        }
    }

    fn build_args_array(&mut self, args: &[Arg]) {
        self.emit(Op::NewArray(0));
        for a in args {
            match a {
                Arg::Item(x) => {
                    self.expr(x);
                    self.emit(Op::ArrayPush);
                }
                Arg::Spread(x) => {
                    self.expr(x);
                    self.emit(Op::ArraySpread);
                }
            }
        }
    }

    fn assign(&mut self, op: AssignOp, target: &Expr, value: &Expr, span: Span) {
        match &target.kind {
            ExprKind::Ident(n) => {
                match op {
                    AssignOp::Assign => self.expr(value),
                    AssignOp::Bin(b) => {
                        self.emit_get(n, span);
                        self.expr(value);
                        self.emit(Self::binop(b));
                    }
                    AssignOp::Log(l) => {
                        self.emit_get(n, span);
                        let j = self.emit(match l {
                            LogOp::And => Op::AndJump(0),
                            LogOp::Or => Op::OrJump(0),
                            LogOp::Nullish => Op::NullishJump(0),
                        });
                        self.expr(value);
                        self.emit(Op::Dup);
                        self.emit_set(n, span);
                        self.patch_here(j);
                        return;
                    }
                }
                self.emit(Op::Dup);
                self.emit_set(n, span);
            }
            ExprKind::Member { obj, name, .. } => {
                let k = self.const_str(name);
                self.expr(obj);
                match op {
                    AssignOp::Assign => {
                        self.expr(value);
                        self.emit(Op::SetProp(k));
                    }
                    AssignOp::Bin(b) => {
                        self.emit(Op::Dup);
                        self.emit(Op::GetProp(k));
                        self.expr(value);
                        self.emit(Self::binop(b));
                        self.emit(Op::SetProp(k));
                    }
                    AssignOp::Log(l) => {
                        self.emit(Op::Dup);
                        self.emit(Op::GetProp(k));
                        let j = self.emit(match l {
                            LogOp::And => Op::AndJump(0),
                            LogOp::Or => Op::OrJump(0),
                            LogOp::Nullish => Op::NullishJump(0),
                        });
                        // stack [obj]; value path
                        self.expr(value);
                        self.emit(Op::SetProp(k));
                        let je = self.emit(Op::Jump(0));
                        self.patch_here(j);
                        // stack [obj, cur]: drop obj
                        self.emit(Op::Swap);
                        self.emit(Op::Pop);
                        self.patch_here(je);
                    }
                }
            }
            ExprKind::Index { obj, idx, .. } => {
                self.expr(obj);
                self.expr(idx);
                match op {
                    AssignOp::Assign => {
                        self.expr(value);
                        self.emit(Op::SetIndex);
                    }
                    AssignOp::Bin(b) => {
                        self.emit(Op::Dup2);
                        self.emit(Op::GetIndex);
                        self.expr(value);
                        self.emit(Self::binop(b));
                        self.emit(Op::SetIndex);
                    }
                    AssignOp::Log(l) => {
                        self.emit(Op::Dup2);
                        self.emit(Op::GetIndex);
                        let j = self.emit(match l {
                            LogOp::And => Op::AndJump(0),
                            LogOp::Or => Op::OrJump(0),
                            LogOp::Nullish => Op::NullishJump(0),
                        });
                        self.expr(value);
                        self.emit(Op::SetIndex);
                        let je = self.emit(Op::Jump(0));
                        self.patch_here(j);
                        // stack [obj, idx, cur] -> [cur]
                        self.emit(Op::Rot3);
                        self.emit(Op::Pop);
                        self.emit(Op::Pop);
                        self.patch_here(je);
                    }
                }
            }
            _ => self.ice(span, "invalid assignment target"),
        }
    }

    fn update(&mut self, inc: bool, prefix: bool, target: &Expr, span: Span) {
        let arith = if inc { Op::Add } else { Op::Sub };
        let one = self.const_num(1.0);
        match &target.kind {
            ExprKind::Ident(n) => {
                self.emit_get(n, span);
                if prefix {
                    self.emit(Op::Const(one));
                    self.emit(arith);
                    self.emit(Op::Dup);
                    self.emit_set(n, span);
                } else {
                    self.emit(Op::Plus);
                    self.emit(Op::Dup);
                    self.emit(Op::Const(one));
                    self.emit(arith);
                    self.emit_set(n, span);
                }
            }
            ExprKind::Member { obj, name, .. } => {
                let k = self.const_str(name);
                self.expr(obj);
                self.emit(Op::Dup);
                self.emit(Op::GetProp(k));
                if prefix {
                    self.emit(Op::Const(one));
                    self.emit(arith);
                    self.emit(Op::SetProp(k));
                } else {
                    self.emit(Op::Plus);
                    let tmp = self.alloc_slot();
                    self.emit(Op::Dup);
                    self.emit(Op::SetLocal(tmp));
                    self.emit(Op::Const(one));
                    self.emit(arith);
                    self.emit(Op::SetProp(k));
                    self.emit(Op::Pop);
                    self.emit(Op::GetLocal(tmp));
                }
            }
            ExprKind::Index { obj, idx, .. } => {
                self.expr(obj);
                self.expr(idx);
                self.emit(Op::Dup2);
                self.emit(Op::GetIndex);
                if prefix {
                    self.emit(Op::Const(one));
                    self.emit(arith);
                    self.emit(Op::SetIndex);
                } else {
                    self.emit(Op::Plus);
                    let tmp = self.alloc_slot();
                    self.emit(Op::Dup);
                    self.emit(Op::SetLocal(tmp));
                    self.emit(Op::Const(one));
                    self.emit(arith);
                    self.emit(Op::SetIndex);
                    self.emit(Op::Pop);
                    self.emit(Op::GetLocal(tmp));
                }
            }
            _ => self.ice(span, "invalid update target"),
        }
    }
}
