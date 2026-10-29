use crate::ast::*;
use crate::diag::*;
use crate::lexer::Span;
use crate::project::{CompilerCfg, Loaded};
use crate::types::*;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

// ---------------- environment structures ----------------

pub enum AliasState {
    Unresolved,
    Resolving,
    Done(Vec<Rc<str>>, Ty),
}

pub struct AliasDef {
    pub name: String,
    pub module: usize,
    pub tparams: Vec<crate::ast::TypeParam>,
    pub texpr: TypeExpr,
    pub state: RefCell<AliasState>,
}

#[derive(Clone)]
pub enum TypeEntry {
    Iface(usize),
    Alias(Rc<AliasDef>),
}

pub enum TopTy<'a> {
    Done(Ty),
    LazyVar(usize, &'a Stmt),
    LazyFn(usize, Rc<FuncDecl>),
    InProgress,
    InFn(Rc<FuncDecl>),
    Namespace(usize),
}

pub struct TopSym<'a> {
    pub ty: RefCell<TopTy<'a>>,
    pub mutable: bool,
    pub module: usize,
}

pub type SymRef<'a> = Rc<TopSym<'a>>;

#[derive(Default)]
pub struct ModEnv<'a> {
    pub types: HashMap<String, TypeEntry>,
    pub vals: HashMap<String, SymRef<'a>>,
    pub exp_types: HashMap<String, TypeEntry>,
    pub exp_vals: HashMap<String, SymRef<'a>>,
    pub import_spans: Vec<(String, Span)>,
    pub used_imports: HashSet<String>,
}

pub struct Local {
    pub id: u32,
    pub declared: Ty,
    pub mutable: bool,
    pub fn_depth: usize,
    pub used: Cell<bool>,
    pub span: Span,
    pub warn_unused: bool,
}

#[derive(Default)]
pub struct Scope {
    pub vars: HashMap<String, Local>,
    pub types: HashMap<String, Rc<str>>,
}

pub struct FnCtx {
    pub ret_expected: Option<Ty>,
    pub ret_hint: Option<Ty>,
    pub returns: Vec<Ty>,
    pub is_async: bool,
    pub top_level: bool,
}

pub type Facts = HashMap<String, Ty>;

pub struct Checker<'a> {
    pub loaded: &'a Loaded,
    pub cfg: &'a CompilerCfg,
    pub tcx: TypeCtx,
    pub next_uid: u32,
    pub mods: Vec<ModEnv<'a>>,
    pub diags: Vec<Diagnostic>,
    pub cur: usize,
    pub scopes: Vec<Scope>,
    pub facts: Facts,
    pub fns: Vec<FnCtx>,
    pub diverged: bool,
    pub chain_null: bool,
    pub loop_depth: usize,
    pub quiet: u32,
    pub local_mut: HashMap<u32, bool>,
    pub iface_decls: HashMap<usize, (usize, &'a InterfaceDecl)>,
    pub iface_state: HashMap<usize, u8>,
    pub fn_types: HashMap<*const FuncDecl, Ty>,
    pub fn_checked: HashSet<*const FuncDecl>,
    pub fn_in_progress: HashSet<*const FuncDecl>,
    pub builtin: HashMap<&'static str, usize>,
    pub mcp_types: HashMap<String, usize>,
    pub checked_var_stmts: HashSet<*const Stmt>,
    pub is_strict: bool,
    pub prelude_mod: usize,
    pub tool_names: HashMap<String, Span>,
    pub warned_unreachable: bool,
    pub switch_depth: usize,
    pub broke: bool,
}

pub fn check_program<'a>(loaded: &'a Loaded, cfg: &'a CompilerCfg) -> (Vec<Diagnostic>, TypeCtx) {
    let mut c = Checker {
        loaded,
        cfg,
        tcx: TypeCtx::default(),
        next_uid: 0,
        mods: Vec::new(),
        diags: Vec::new(),
        cur: 0,
        scopes: Vec::new(),
        facts: HashMap::new(),
        fns: Vec::new(),
        diverged: false,
        chain_null: false,
        loop_depth: 0,
        quiet: 0,
        local_mut: HashMap::new(),
        iface_decls: HashMap::new(),
        iface_state: HashMap::new(),
        fn_types: HashMap::new(),
        fn_checked: HashSet::new(),
        fn_in_progress: HashSet::new(),
        builtin: HashMap::new(),
        mcp_types: HashMap::new(),
        checked_var_stmts: HashSet::new(),
        is_strict: cfg.strict_types,
        prelude_mod: 0,
        tool_names: HashMap::new(),
        warned_unreachable: false,
        switch_depth: 0,
        broke: false,
    };
    for _ in 0..loaded.modules.len() {
        c.mods.push(ModEnv::default());
    }
    for id in 0..loaded.modules.len() {
        c.check_module(id);
    }
    c.check_header_impls();
    (c.diags, c.tcx)
}

impl<'a> Checker<'a> {
    // ---------------- diagnostics ----------------
    pub fn err(&mut self, span: Span, msg: impl Into<String>) {
        if self.quiet > 0 {
            return;
        }
        let file = self.loaded.modules[self.cur].display.clone();
        self.diags.push(Diagnostic::error(&file, span, msg));
    }
    pub fn warn(&mut self, span: Span, msg: impl Into<String>) {
        if self.quiet > 0 || self.loaded.modules[self.cur].is_std {
            return;
        }
        let file = self.loaded.modules[self.cur].display.clone();
        self.diags.push(Diagnostic::warning(&file, span, msg));
    }
    pub fn uid(&mut self) -> u32 {
        self.next_uid += 1;
        self.next_uid
    }
    pub fn show(&self, t: &Ty) -> String {
        self.tcx.show(t)
    }

    // ---------------- module setup ----------------
    fn is_header_all_exported(&self, id: usize) -> bool {
        self.loaded.modules[id].module.is_header
    }

    fn new_tparam_names(&mut self, tps: &[crate::ast::TypeParam]) -> Vec<Rc<str>> {
        tps.iter()
            .map(|tp| {
                let u = self.uid();
                Rc::from(format!("{}#{}", tp.name, u))
            })
            .collect()
    }

    pub fn check_module(&mut self, id: usize) {
        self.cur = id;
        self.scopes.clear();
        self.facts.clear();
        self.fns.clear();
        self.diverged = false;
        let loaded = self.loaded;
        let src = &loaded.modules[id];
        let all_exported = self.is_header_all_exported(id);
        let is_prelude = src.key == "std/prelude";
        if is_prelude {
            self.prelude_mod = id;
        }
        // 0. an implementation sees the types declared by its own header
        for (hid, h) in loaded.modules.iter().enumerate() {
            if h.impl_id == Some(id) && hid < id {
                let entries: Vec<(String, TypeEntry)> = self.mods[hid].exp_types.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                for (k, v) in entries {
                    self.mods[id].types.entry(k).or_insert(v);
                }
            }
        }
        // 1. prelude globals visible everywhere (except prelude itself)
        // (resolved through lookup fallbacks)

        // 2. imports
        for s in &src.module.body {
            if let StmtKind::Import { specs, ns, from, type_only } = &s.kind {
                let Some(&dep) = src.deps.get(from) else { continue };
                let dep_id = if loaded.modules[dep].is_std || !loaded.modules[dep].module.is_header { dep } else { dep };
                if let Some(nsname) = ns {
                    let sym = Rc::new(TopSym { ty: RefCell::new(TopTy::Namespace(dep_id)), mutable: false, module: dep_id });
                    self.mods[id].vals.insert(nsname.clone(), sym);
                    self.mods[id].import_spans.push((nsname.clone(), s.span));
                    continue;
                }
                for sp in specs {
                    let mut found = false;
                    if let Some(t) = self.mods[dep].exp_types.get(&sp.name).cloned() {
                        self.mods[id].types.insert(sp.alias.clone(), t);
                        found = true;
                    }
                    if !*type_only {
                        // value import: for headers with an implementation, values are those declared by the header
                        if let Some(v) = self.mods[dep].exp_vals.get(&sp.name).cloned() {
                            self.mods[id].vals.insert(sp.alias.clone(), v);
                            found = true;
                        }
                    }
                    if !found {
                        self.cur = id;
                        self.err(s.span, format!("module '{}' has no exported member '{}'", from, sp.name));
                    } else {
                        self.mods[id].import_spans.push((sp.alias.clone(), s.span));
                    }
                }
            }
        }

        // 3. declare types (interfaces, aliases)
        for s in &src.module.body {
            match &s.kind {
                StmtKind::Interface(d) => {
                    if self.mods[id].types.contains_key(&d.name) && !self.mods[id].exp_types.contains_key(&d.name) {
                        // may shadow imported name; only error for local duplicates
                    }
                    if self.local_type_declared(id, &d.name) {
                        self.err(s.span, format!("duplicate type declaration '{}'", d.name));
                        continue;
                    }
                    let tps = self.new_tparam_names(&d.tparams);
                    let iid = self.tcx.ifaces.len();
                    self.tcx.ifaces.push(IfaceInfo { name: d.name.clone(), tparams: tps, body: None });
                    self.iface_decls.insert(iid, (id, d));
                    self.mods[id].types.insert(d.name.clone(), TypeEntry::Iface(iid));
                    self.mark_local_type(id, &d.name);
                    if d.exported || all_exported {
                        self.mods[id].exp_types.insert(d.name.clone(), TypeEntry::Iface(iid));
                    }
                    if src.key == "std/prelude" {
                        match d.name.as_str() {
                            "ArrayMethods" => {
                                self.builtin.insert("array", iid);
                            }
                            "StringMethods" => {
                                self.builtin.insert("string", iid);
                            }
                            "NumberMethods" => {
                                self.builtin.insert("number", iid);
                            }
                            "BooleanMethods" => {
                                self.builtin.insert("boolean", iid);
                            }
                            _ => {}
                        }
                    }
                    if src.key == "std/mcp" {
                        self.mcp_types.insert(d.name.clone(), iid);
                    }
                }
                StmtKind::TypeAlias { name, tparams, ty, exported } => {
                    if self.local_type_declared(id, name) {
                        self.err(s.span, format!("duplicate type declaration '{}'", name));
                        continue;
                    }
                    let a = Rc::new(AliasDef { name: name.clone(), module: id, tparams: tparams.clone(), texpr: ty.clone(), state: RefCell::new(AliasState::Unresolved) });
                    self.mods[id].types.insert(name.clone(), TypeEntry::Alias(a.clone()));
                    self.mark_local_type(id, name);
                    if *exported || all_exported {
                        self.mods[id].exp_types.insert(name.clone(), TypeEntry::Alias(a));
                    }
                }
                _ => {}
            }
        }
        // 4. interface bodies
        let iface_ids: Vec<usize> = self.iface_decls.iter().filter(|(_, (m, _))| *m == id).map(|(i, _)| *i).collect();
        let mut iface_ids = iface_ids;
        iface_ids.sort();
        for iid in iface_ids {
            self.ensure_iface(iid);
        }
        // 5. value declarations
        for s in &src.module.body {
            match &s.kind {
                StmtKind::Func(f) => {
                    let name = f.name.clone().unwrap();
                    if self.mods[id].vals.get(&name).map_or(false, |v| v.module == id && !matches!(*v.ty.borrow(), TopTy::Namespace(_))) && self.local_value_declared(id, &name) {
                        self.err(f.span, format!("duplicate declaration '{}'", name));
                        continue;
                    }
                    let sym = Rc::new(TopSym { ty: RefCell::new(TopTy::LazyFn(id, f.clone())), mutable: false, module: id });
                    self.mods[id].vals.insert(name.clone(), sym.clone());
                    self.mark_local_value(id, &name);
                    if f.exported || all_exported {
                        self.mods[id].exp_vals.insert(name, sym);
                    }
                }
                StmtKind::Var { kind, pattern, ty, exported, .. } => {
                    let mut names = Vec::new();
                    pattern_names(pattern, &mut names);
                    for (n, sp) in names {
                        if self.local_value_declared(id, &n) {
                            self.err(sp, format!("duplicate declaration '{}'", n));
                            continue;
                        }
                        let sym = Rc::new(TopSym { ty: RefCell::new(TopTy::LazyVar(id, s)), mutable: *kind == VarKind::Let, module: id });
                        let _ = ty;
                        self.mods[id].vals.insert(n.clone(), sym.clone());
                        self.mark_local_value(id, &n);
                        if *exported || all_exported {
                            self.mods[id].exp_vals.insert(n, sym);
                        }
                    }
                }
                StmtKind::DeclareFunc { name, tparams, params, ret, exported } => {
                    let t = self.declared_fn_type(tparams, params, ret);
                    let sym = Rc::new(TopSym { ty: RefCell::new(TopTy::Done(t)), mutable: false, module: id });
                    self.mods[id].vals.insert(name.clone(), sym.clone());
                    self.mark_local_value(id, name);
                    if *exported || all_exported {
                        self.mods[id].exp_vals.insert(name.clone(), sym);
                    }
                }
                StmtKind::DeclareConst { name, ty, exported } => {
                    let t = self.resolve_type(ty);
                    let sym = Rc::new(TopSym { ty: RefCell::new(TopTy::Done(t)), mutable: false, module: id });
                    self.mods[id].vals.insert(name.clone(), sym.clone());
                    self.mark_local_value(id, name);
                    if *exported || all_exported {
                        self.mods[id].exp_vals.insert(name.clone(), sym);
                    }
                }
                _ => {}
            }
        }
        // export { a, b as c }
        for s in &src.module.body {
            if let StmtKind::ExportNames(specs) = &s.kind {
                for sp in specs {
                    let mut found = false;
                    if let Some(v) = self.mods[id].vals.get(&sp.name).cloned() {
                        self.mods[id].exp_vals.insert(sp.alias.clone(), v);
                        found = true;
                    }
                    if let Some(t) = self.mods[id].types.get(&sp.name).cloned() {
                        self.mods[id].exp_types.insert(sp.alias.clone(), t);
                        found = true;
                    }
                    if !found {
                        self.err(s.span, format!("cannot export '{}': not declared in this module", sp.name));
                    }
                }
            }
        }

        // 6. check statements in order
        if !src.is_std {
            self.fns.push(FnCtx { ret_expected: None, ret_hint: None, returns: vec![], is_async: true, top_level: true });
            self.scopes.push(Scope::default());
            for s in &src.module.body {
                self.check_top_stmt(s);
            }
            self.pop_scope();
            self.fns.pop();
            self.warn_unused_imports(id);
        }
    }

    fn warn_unused_imports(&mut self, id: usize) {
        let spans = self.mods[id].import_spans.clone();
        for (name, span) in spans {
            if !self.mods[id].used_imports.contains(&name) && !name.starts_with('_') {
                // type usage is tracked through types map access
                self.warn(span, format!("unused import '{}'", name));
            }
        }
    }

    fn local_type_declared(&self, id: usize, name: &str) -> bool {
        self.mods[id].used_imports.contains(&format!("\0t:{}", name))
    }
    fn mark_local_type(&mut self, id: usize, name: &str) {
        self.mods[id].used_imports.insert(format!("\0t:{}", name));
    }
    fn local_value_declared(&self, id: usize, name: &str) -> bool {
        self.mods[id].used_imports.contains(&format!("\0v:{}", name))
    }
    fn mark_local_value(&mut self, id: usize, name: &str) {
        self.mods[id].used_imports.insert(format!("\0v:{}", name));
    }

    pub fn mark_import_used(&mut self, name: &str) {
        let cur = self.cur;
        self.mods[cur].used_imports.insert(name.to_string());
    }

    fn declared_fn_type(&mut self, tparams: &[crate::ast::TypeParam], params: &[FnTypeParam], ret: &TypeExpr) -> Ty {
        let names = self.new_tparam_names(tparams);
        let mut scope = Scope::default();
        for (tp, n) in tparams.iter().zip(names.iter()) {
            scope.types.insert(tp.name.clone(), n.clone());
        }
        self.scopes.push(scope);
        let mut tps = Vec::new();
        for (tp, n) in tparams.iter().zip(names.iter()) {
            let c = tp.constraint.as_ref().map(|c| self.resolve_type(c));
            if let Some(c) = &c {
                self.tcx.constraints.insert(n.clone(), c.clone());
            }
            tps.push(TParam { name: n.clone(), constraint: c });
        }
        let ps: Vec<ParamTy> = params
            .iter()
            .map(|p| {
                let t = self.resolve_type(&p.ty);
                ParamTy { name: Rc::from(p.name.as_str()), ty: self.rest_elem(t, p.rest, decl_span(&p.ty)), optional: p.optional, rest: p.rest }
            })
            .collect();
        let r = self.resolve_type(ret);
        self.scopes.pop();
        Ty::Func(Rc::new(FnTy { tparams: tps, params: ps, ret: r }))
    }

    pub fn rest_elem(&mut self, t: Ty, rest: bool, span: Span) -> Ty {
        if !rest {
            return t;
        }
        match t {
            Ty::Array(e) => (*e).clone(),
            Ty::Error | Ty::Any => Ty::Any,
            _ => {
                self.err(span, "a rest parameter must have an array type");
                Ty::Error
            }
        }
    }

    // ---------------- type resolution ----------------
    pub fn ensure_iface(&mut self, iid: usize) {
        match self.iface_state.get(&iid) {
            Some(2) => return,
            Some(1) => {
                // circular extends or self-reference through extends
                return;
            }
            _ => {}
        }
        let Some(&(module, decl)) = self.iface_decls.get(&iid) else { return };
        self.iface_state.insert(iid, 1);
        let saved_cur = self.cur;
        let saved_scopes = std::mem::take(&mut self.scopes);
        self.cur = module;
        let tps = self.tcx.ifaces[iid].tparams.clone();
        let mut scope = Scope::default();
        for (tp, n) in decl.tparams.iter().zip(tps.iter()) {
            scope.types.insert(tp.name.clone(), n.clone());
        }
        self.scopes.push(scope);
        for (tp, n) in decl.tparams.iter().zip(tps.iter()) {
            if let Some(c) = &tp.constraint {
                let ct = self.resolve_type(c);
                self.tcx.constraints.insert(n.clone(), ct);
            }
        }
        let mut obj = ObjTy::default();
        for ext in &decl.extends {
            let t = self.resolve_type(ext);
            let r = self.tcx.resolve(&t);
            if let Ty::Obj(o) = r {
                for (k, p) in o.props.iter() {
                    obj.props.insert(k.clone(), p.clone());
                }
                if o.index.is_some() {
                    obj.index = o.index.clone();
                }
            } else if !matches!(t, Ty::Error) {
                let sp = decl_span(ext);
                self.err(sp, "an interface can only extend object types");
            }
        }
        for p in &decl.props {
            let t = self.resolve_type(&p.ty);
            if p.index {
                obj.index = Some(t);
            } else {
                obj.props.insert(p.name.clone(), Prop { ty: t, optional: p.optional });
            }
        }
        self.tcx.ifaces[iid].body = Some(Rc::new(obj));
        self.scopes = saved_scopes;
        self.cur = saved_cur;
        self.iface_state.insert(iid, 2);
    }

    fn lookup_type_entry(&mut self, name: &str) -> Option<TypeEntry> {
        if let Some((ns, ty)) = name.split_once('.') {
            let sym = self.mods[self.cur].vals.get(ns).cloned()?;
            let m = if let TopTy::Namespace(m) = &*sym.ty.borrow() { *m } else { return None };
            self.mods[self.cur].used_imports.insert(ns.to_string());
            return self.mods[m].exp_types.get(ty).cloned();
        }
        if let Some(e) = self.mods[self.cur].types.get(name).cloned() {
            self.mods[self.cur].used_imports.insert(name.to_string());
            return Some(e);
        }
        self.mods[self.prelude_mod].exp_types.get(name).cloned()
    }

    pub fn resolve_type(&mut self, te: &TypeExpr) -> Ty {
        match te {
            TypeExpr::LitStr(s) => Ty::LitStr(Rc::from(s.as_str())),
            TypeExpr::LitNum(n) => Ty::LitNum(*n),
            TypeExpr::LitBool(b) => Ty::LitBool(*b),
            TypeExpr::Null => Ty::Null,
            TypeExpr::Array(e) => Ty::Array(Rc::new(self.resolve_type(e))),
            TypeExpr::Tuple(v) => {
                let items: Vec<Ty> = v.iter().map(|t| self.resolve_type(t)).collect();
                Ty::Tuple(Rc::new(items))
            }
            TypeExpr::Union(v) => {
                let items: Vec<Ty> = v.iter().map(|t| self.resolve_type(t)).collect();
                mk_union(items)
            }
            TypeExpr::Object(props, _) => {
                let mut o = ObjTy::default();
                for p in props {
                    let t = self.resolve_type(&p.ty);
                    if p.index {
                        o.index = Some(t);
                    } else {
                        o.props.insert(p.name.clone(), Prop { ty: t, optional: p.optional });
                    }
                }
                Ty::Obj(Rc::new(o))
            }
            TypeExpr::Func { tparams, params, ret, .. } => {
                let names = self.new_tparam_names(tparams);
                let mut scope = Scope::default();
                for (tp, n) in tparams.iter().zip(names.iter()) {
                    scope.types.insert(tp.name.clone(), n.clone());
                }
                self.scopes.push(scope);
                let mut tps = Vec::new();
                for (tp, n) in tparams.iter().zip(names.iter()) {
                    let c = tp.constraint.as_ref().map(|c| self.resolve_type(c));
                    if let Some(c) = &c {
                        self.tcx.constraints.insert(n.clone(), c.clone());
                    }
                    tps.push(TParam { name: n.clone(), constraint: c });
                }
                let ps: Vec<ParamTy> = params
                    .iter()
                    .map(|p| {
                        let t = self.resolve_type(&p.ty);
                        ParamTy { name: Rc::from(p.name.as_str()), ty: self.rest_elem(t, p.rest, decl_span(&p.ty)), optional: p.optional, rest: p.rest }
                    })
                    .collect();
                let r = self.resolve_type(ret);
                self.scopes.pop();
                Ty::Func(Rc::new(FnTy { tparams: tps, params: ps, ret: r }))
            }
            TypeExpr::Named { name, args, span } => self.resolve_named(name, args, *span),
        }
    }

    fn resolve_named(&mut self, name: &str, args: &[TypeExpr], span: Span) -> Ty {
        // generic parameters in scope
        for sc in self.scopes.iter().rev() {
            if let Some(u) = sc.types.get(name) {
                if !args.is_empty() {
                    let u = u.clone();
                    self.err(span, format!("type parameter '{}' takes no type arguments", name));
                    return Ty::Param(u);
                }
                return Ty::Param(u.clone());
            }
        }
        let arity_check = |s: &mut Self, expected: usize| -> bool {
            if args.len() != expected {
                s.err(span, format!("type '{}' requires {} type argument(s), found {}", name, expected, args.len()));
                false
            } else {
                true
            }
        };
        match name {
            "number" => return Ty::Number,
            "string" => return Ty::Str,
            "boolean" => return Ty::Bool,
            "void" => return Ty::Void,
            "unknown" => return Ty::Unknown,
            "never" => return Ty::Never,
            "any" => {
                if self.is_strict && !self.loaded.modules[self.cur].is_std {
                    self.err(span, "type 'any' is not allowed when strict_types is enabled; use 'unknown' or a specific type");
                }
                return Ty::Any;
            }
            "Array" => {
                if !arity_check(self, 1) {
                    return Ty::Error;
                }
                return Ty::Array(Rc::new(self.resolve_type(&args[0])));
            }
            "Promise" => {
                if !arity_check(self, 1) {
                    return Ty::Error;
                }
                let inner = self.resolve_type(&args[0]);
                return mk_promise(inner);
            }
            "Record" => {
                if !arity_check(self, 2) {
                    return Ty::Error;
                }
                let k = self.resolve_type(&args[0]);
                if !matches!(k, Ty::Str | Ty::Error | Ty::Any) {
                    self.err(span, "Record keys must be of type 'string'");
                }
                let v = self.resolve_type(&args[1]);
                return Ty::Obj(Rc::new(ObjTy { props: Default::default(), index: Some(v) }));
            }
            _ => {}
        }
        // user lookup (module types, then prelude)
        let Some(entry) = self.lookup_type_entry(name) else {
            self.err(span, format!("cannot find type '{}'", name));
            return Ty::Error;
        };
        match entry {
            TypeEntry::Iface(iid) => {
                let n = self.tcx.ifaces[iid].tparams.len();
                if args.len() != n {
                    self.err(span, format!("type '{}' requires {} type argument(s), found {}", name, n, args.len()));
                    return Ty::Error;
                }
                let targs: Vec<Ty> = args.iter().map(|a| self.resolve_type(a)).collect();
                Ty::Ref(iid, Rc::new(targs))
            }
            TypeEntry::Alias(a) => {
                if args.len() != a.tparams.len() {
                    self.err(span, format!("type '{}' requires {} type argument(s), found {}", name, a.tparams.len(), args.len()));
                    return Ty::Error;
                }
                let targs: Vec<Ty> = args.iter().map(|x| self.resolve_type(x)).collect();
                self.resolve_alias(&a, targs, span)
            }
        }
    }

    fn resolve_alias(&mut self, a: &Rc<AliasDef>, targs: Vec<Ty>, span: Span) -> Ty {
        let state = std::mem::replace(&mut *a.state.borrow_mut(), AliasState::Resolving);
        let (names, body) = match state {
            AliasState::Done(n, b) => {
                *a.state.borrow_mut() = AliasState::Done(n.clone(), b.clone());
                (n, b)
            }
            AliasState::Resolving => {
                *a.state.borrow_mut() = AliasState::Resolving;
                self.err(span, format!("type alias '{}' circularly references itself (use an interface for recursive types)", a.name));
                return Ty::Error;
            }
            AliasState::Unresolved => {
                let saved_cur = self.cur;
                let saved_scopes = std::mem::take(&mut self.scopes);
                self.cur = a.module;
                let names = self.new_tparam_names(&a.tparams);
                let mut scope = Scope::default();
                for (tp, n) in a.tparams.iter().zip(names.iter()) {
                    scope.types.insert(tp.name.clone(), n.clone());
                }
                self.scopes.push(scope);
                let body = self.resolve_type(&a.texpr);
                self.scopes = saved_scopes;
                self.cur = saved_cur;
                *a.state.borrow_mut() = AliasState::Done(names.clone(), body.clone());
                (names, body)
            }
        };
        let map: HashMap<Rc<str>, Ty> = names.into_iter().zip(targs.into_iter()).collect();
        subst(&body, &map)
    }

    // ---------------- scopes ----------------
    pub fn push_scope(&mut self) {
        self.scopes.push(Scope::default());
    }

    pub fn pop_scope(&mut self) {
        let sc = self.scopes.pop().unwrap();
        let mut unused: Vec<(String, Span)> = sc.vars.iter().filter(|(n, l)| l.warn_unused && !l.used.get() && !n.starts_with('_')).map(|(n, l)| (n.clone(), l.span)).collect();
        unused.sort_by_key(|(_, s)| (s.line, s.col));
        for (n, s) in unused {
            self.warn(s, format!("unused variable '{}'", n));
        }
    }

    pub fn fn_depth(&self) -> usize {
        self.fns.len()
    }

    pub fn declare_local(&mut self, name: &str, ty: Ty, mutable: bool, span: Span, warn_unused: bool) -> u32 {
        let id = self.uid();
        self.local_mut.insert(id, mutable);
        let depth = self.fn_depth();
        let sc = self.scopes.last_mut().unwrap();
        if sc.vars.contains_key(name) {
            self.err(span, format!("'{}' is already declared in this scope", name));
        }
        let sc = self.scopes.last_mut().unwrap();
        sc.vars.insert(name.to_string(), Local { id, declared: ty, mutable, fn_depth: depth, used: Cell::new(false), span, warn_unused });
        id
    }

    pub fn find_local(&self, name: &str) -> Option<&Local> {
        for sc in self.scopes.iter().rev() {
            if let Some(l) = sc.vars.get(name) {
                return Some(l);
            }
        }
        None
    }

    // ---------------- top-level symbols ----------------
    /// Resolve the type of a module-level symbol, performing lazy inference.
    pub fn sym_type(&mut self, sym: &SymRef<'a>, span: Span) -> Ty {
        let state = std::mem::replace(&mut *sym.ty.borrow_mut(), TopTy::InProgress);
        match state {
            TopTy::Done(t) => {
                *sym.ty.borrow_mut() = TopTy::Done(t.clone());
                t
            }
            TopTy::Namespace(m) => {
                *sym.ty.borrow_mut() = TopTy::Namespace(m);
                Ty::Error
            }
            TopTy::InProgress => {
                *sym.ty.borrow_mut() = TopTy::InProgress;
                self.err(span, "this declaration is used while its own type is being inferred; add a type annotation");
                Ty::Error
            }
            TopTy::InFn(f) => {
                let early = self.fn_types.get(&Rc::as_ptr(&f)).cloned();
                *sym.ty.borrow_mut() = TopTy::InFn(f);
                match early {
                    Some(t) => t,
                    None => {
                        self.err(span, "this function needs an explicit return type annotation because it is used while its own type is being inferred");
                        Ty::Error
                    }
                }
            }
            TopTy::LazyFn(m, f) => {
                *sym.ty.borrow_mut() = TopTy::InFn(f.clone());
                let t = self.in_module(m, |s| s.top_fn_type(&f));
                *sym.ty.borrow_mut() = TopTy::Done(t.clone());
                t
            }
            TopTy::LazyVar(m, stmt) => {
                self.in_module(m, |s| s.check_top_var(stmt));
                let cur = std::mem::replace(&mut *sym.ty.borrow_mut(), TopTy::InProgress);
                match cur {
                    TopTy::Done(t) => {
                        *sym.ty.borrow_mut() = TopTy::Done(t.clone());
                        t
                    }
                    other => {
                        *sym.ty.borrow_mut() = other;
                        Ty::Error
                    }
                }
            }
        }
    }

    pub fn in_module<T>(&mut self, m: usize, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = (
            self.cur,
            std::mem::take(&mut self.scopes),
            std::mem::take(&mut self.facts),
            std::mem::take(&mut self.fns),
            self.diverged,
            self.chain_null,
            self.loop_depth,
        );
        self.cur = m;
        self.fns.push(FnCtx { ret_expected: None, ret_hint: None, returns: vec![], is_async: true, top_level: true });
        self.scopes.push(Scope::default());
        self.diverged = false;
        self.loop_depth = 0;
        let r = f(self);
        self.cur = saved.0;
        self.scopes = saved.1;
        self.facts = saved.2;
        self.fns = saved.3;
        self.diverged = saved.4;
        self.chain_null = saved.5;
        self.loop_depth = saved.6;
        r
    }

    /// Compute (and cache) the type of a top-level function, checking its body if the return type must be inferred.
    pub fn top_fn_type(&mut self, f: &Rc<FuncDecl>) -> Ty {
        let key = Rc::as_ptr(f);
        if let Some(t) = self.fn_types.get(&key) {
            return t.clone();
        }
        if self.fn_in_progress.contains(&key) {
            self.err(f.span, format!("function '{}' needs an explicit return type annotation because it calls itself recursively", f.name.clone().unwrap_or_default()));
            return Ty::Error;
        }
        self.fn_in_progress.insert(key);
        let t = self.check_function(f, None);
        self.fn_in_progress.remove(&key);
        self.fn_types.insert(key, t.clone());
        t
    }

    pub fn check_top_var(&mut self, s: &'a Stmt) {
        let key = s as *const Stmt;
        if self.checked_var_stmts.contains(&key) {
            return;
        }
        self.checked_var_stmts.insert(key);
        let m = self.cur;
        // mark all names as in progress
        if let StmtKind::Var { kind, pattern, ty, init, .. } = &s.kind {
            let mut names = Vec::new();
            pattern_names(pattern, &mut names);
            for (n, _) in &names {
                if let Some(sym) = self.mods[m].vals.get(n) {
                    if matches!(*sym.ty.borrow(), TopTy::LazyVar(..)) {
                        *sym.ty.borrow_mut() = TopTy::InProgress;
                    }
                }
            }
            let bound = self.check_var_decl_types(*kind, pattern, ty.as_ref(), init.as_ref(), s.span);
            for (n, t, _) in bound {
                if let Some(sym) = self.mods[m].vals.get(&n).cloned() {
                    if sym.module == m {
                        *sym.ty.borrow_mut() = TopTy::Done(t);
                    }
                }
            }
        }
    }

    fn check_top_stmt(&mut self, s: &'a Stmt) {
        match &s.kind {
            StmtKind::Var { .. } => self.check_top_var(s),
            StmtKind::Func(f) => {
                let key = Rc::as_ptr(f);
                // ensure signature known
                let name = f.name.clone().unwrap();
                if let Some(sym) = self.mods[self.cur].vals.get(&name).cloned() {
                    if sym.module == self.cur {
                        self.sym_type(&sym, f.span);
                    }
                }
                let _ = key;
                if !f.decorators.is_empty() {
                    self.check_decorators(f);
                }
            }
            StmtKind::Interface(_) | StmtKind::TypeAlias { .. } => {
                // resolve aliases eagerly to surface errors
                if let StmtKind::TypeAlias { name, tparams, .. } = &s.kind {
                    if let Some(TypeEntry::Alias(a)) = self.mods[self.cur].types.get(name).cloned() {
                        let args: Vec<Ty> = tparams.iter().map(|_| Ty::Any).collect();
                        self.resolve_alias(&a, args, s.span);
                    }
                }
            }
            StmtKind::Import { .. } | StmtKind::ExportNames(_) | StmtKind::DeclareFunc { .. } | StmtKind::DeclareConst { .. } => {}
            StmtKind::Test { body, .. } => {
                if self.loaded.modules[self.cur].module.is_header {
                    self.err(s.span, "test blocks are not allowed in header files");
                    return;
                }
                self.fns.push(FnCtx { ret_expected: Some(Ty::Void), ret_hint: None, returns: vec![], is_async: true, top_level: false });
                let saved_div = self.diverged;
                self.diverged = false;
                let saved_facts = std::mem::take(&mut self.facts);
                self.push_scope();
                self.check_stmts(body);
                self.pop_scope();
                self.facts = saved_facts;
                self.diverged = saved_div;
                self.fns.pop();
            }
            _ => {
                self.check_stmt(s);
            }
        }
    }

    // ---------------- header conformance ----------------
    pub fn check_header_impls(&mut self) {
        let loaded = self.loaded;
        for (hid, h) in loaded.modules.iter().enumerate() {
            let Some(iid) = h.impl_id else { continue };
            if h.is_std {
                continue;
            }
            self.cur = hid;
            for s in &h.module.body {
                let (name, hty, span) = match &s.kind {
                    StmtKind::DeclareFunc { name, .. } | StmtKind::DeclareConst { name, .. } => {
                        let Some(sym) = self.mods[hid].vals.get(name).cloned() else { continue };
                        let t = self.sym_type(&sym, s.span);
                        (name.clone(), t, s.span)
                    }
                    _ => continue,
                };
                let Some(isym) = self.mods[iid].exp_vals.get(&name).cloned() else {
                    self.err(span, format!("implementation '{}' does not export '{}' declared in this header", loaded.modules[iid].display, name));
                    continue;
                };
                self.cur = iid;
                let ity = self.sym_type(&isym, span);
                self.cur = hid;
                if !self.tcx.assignable(&ity, &hty) {
                    let (a, b) = (self.show(&ity), self.show(&hty));
                    self.err(span, format!("implementation of '{}' has type '{}' which does not match the header's '{}'", name, a, b));
                }
            }
        }
    }
}

pub fn pattern_names(p: &Pattern, out: &mut Vec<(String, Span)>) {
    match p {
        Pattern::Ident(n, s) => out.push((n.clone(), *s)),
        Pattern::Object { props, rest, span } => {
            for pr in props {
                pattern_names(&pr.target, out);
            }
            if let Some(r) = rest {
                out.push((r.clone(), *span));
            }
        }
        Pattern::Array { elems, rest, span } => {
            for e in elems.iter().flatten() {
                pattern_names(&e.target, out);
            }
            if let Some(r) = rest {
                out.push((r.clone(), *span));
            }
        }
    }
}

pub fn decl_span(t: &TypeExpr) -> Span {
    match t {
        TypeExpr::Named { span, .. } | TypeExpr::Object(_, span) | TypeExpr::Func { span, .. } => *span,
        _ => Span::default(),
    }
}
