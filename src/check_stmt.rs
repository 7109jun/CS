use crate::ast::*;
use crate::check_expr::*;
use crate::checker::*;
use crate::lexer::Span;
use crate::types::*;
use serde_json::{json, Map, Value as Json};
use std::collections::HashSet;
use std::rc::Rc;

// ---------------- syntactic helpers ----------------

pub fn collect_assigned_stmts(stmts: &[Stmt], out: &mut HashSet<String>) {
    for s in stmts {
        collect_assigned_stmt(s, out);
    }
}

fn collect_assigned_stmt(s: &Stmt, out: &mut HashSet<String>) {
    match &s.kind {
        StmtKind::Var { init, .. } => {
            if let Some(e) = init {
                collect_assigned_expr(e, out);
            }
        }
        StmtKind::Expr(e) | StmtKind::Throw(e) => collect_assigned_expr(e, out),
        StmtKind::Block(b) => collect_assigned_stmts(b, out),
        StmtKind::If(c, a, b) => {
            collect_assigned_expr(c, out);
            collect_assigned_stmt(a, out);
            if let Some(b) = b {
                collect_assigned_stmt(b, out);
            }
        }
        StmtKind::While(c, b) | StmtKind::DoWhile(b, c) => {
            collect_assigned_expr(c, out);
            collect_assigned_stmt(b, out);
        }
        StmtKind::For { init, cond, update, body } => {
            if let Some(i) = init {
                collect_assigned_stmt(i, out);
            }
            if let Some(c) = cond {
                collect_assigned_expr(c, out);
            }
            if let Some(u) = update {
                collect_assigned_expr(u, out);
            }
            collect_assigned_stmt(body, out);
        }
        StmtKind::ForOf { iter, body, .. } => {
            collect_assigned_expr(iter, out);
            collect_assigned_stmt(body, out);
        }
        StmtKind::Switch(d, cases) => {
            collect_assigned_expr(d, out);
            for c in cases {
                if let Some(t) = &c.test {
                    collect_assigned_expr(t, out);
                }
                collect_assigned_stmts(&c.body, out);
            }
        }
        StmtKind::Return(Some(e)) => collect_assigned_expr(e, out),
        StmtKind::Try { block, catch, finally } => {
            collect_assigned_stmts(block, out);
            if let Some((_, c)) = catch {
                collect_assigned_stmts(c, out);
            }
            if let Some(f) = finally {
                collect_assigned_stmts(f, out);
            }
        }
        StmtKind::Func(f) => collect_assigned_stmts(&f.body, out),
        _ => {}
    }
}

fn target_root(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Ident(n) => Some(n.clone()),
        ExprKind::Member { obj, .. } | ExprKind::Index { obj, .. } => target_root(obj),
        _ => None,
    }
}

fn collect_assigned_expr(e: &Expr, out: &mut HashSet<String>) {
    match &e.kind {
        ExprKind::Assign { target, value, .. } => {
            if let Some(n) = target_root(target) {
                out.insert(n);
            }
            collect_assigned_expr(target, out);
            collect_assigned_expr(value, out);
        }
        ExprKind::Update { target, .. } => {
            if let Some(n) = target_root(target) {
                out.insert(n);
            }
        }
        ExprKind::Template(segs) => {
            for s in segs {
                if let TemplateSeg::Expr(x) = s {
                    collect_assigned_expr(x, out);
                }
            }
        }
        ExprKind::Array(items) => {
            for i in items {
                match i {
                    ArrElem::Item(x) | ArrElem::Spread(x) => collect_assigned_expr(x, out),
                }
            }
        }
        ExprKind::Object(props) => {
            for p in props {
                match p {
                    ObjProp::KV(_, x) | ObjProp::Spread(x) => collect_assigned_expr(x, out),
                }
            }
        }
        ExprKind::Func(f) => collect_assigned_stmts(&f.body, out),
        ExprKind::Unary(_, x) | ExprKind::Await(x) | ExprKind::TypeOf(x) | ExprKind::As(x, _) | ExprKind::NonNull(x) | ExprKind::Chain(x) => collect_assigned_expr(x, out),
        ExprKind::Binary(_, a, b) | ExprKind::Logical(_, a, b) => {
            collect_assigned_expr(a, out);
            collect_assigned_expr(b, out);
        }
        ExprKind::Cond(a, b, c) => {
            collect_assigned_expr(a, out);
            collect_assigned_expr(b, out);
            collect_assigned_expr(c, out);
        }
        ExprKind::Call { callee, args, .. } => {
            collect_assigned_expr(callee, out);
            for a in args {
                match a {
                    Arg::Item(x) | Arg::Spread(x) => collect_assigned_expr(x, out),
                }
            }
        }
        ExprKind::Member { obj, .. } => collect_assigned_expr(obj, out),
        ExprKind::Index { obj, idx, .. } => {
            collect_assigned_expr(obj, out);
            collect_assigned_expr(idx, out);
        }
        _ => {}
    }
}

/// True if `stmts` contains a `break` that targets the enclosing loop/switch.
pub fn contains_break(stmts: &[Stmt]) -> bool {
    fn walk(s: &Stmt) -> bool {
        match &s.kind {
            StmtKind::Break => true,
            StmtKind::Block(b) => b.iter().any(walk),
            StmtKind::If(_, a, b) => walk(a) || b.as_ref().map_or(false, |b| walk(b)),
            StmtKind::Try { block, catch, finally } => block.iter().any(walk) || catch.as_ref().map_or(false, |(_, c)| c.iter().any(walk)) || finally.as_ref().map_or(false, |f| f.iter().any(walk)),
            // nested loops/switches own their breaks
            _ => false,
        }
    }
    stmts.iter().any(walk)
}

pub fn const_eval(e: &Expr) -> Option<Json> {
    match &e.kind {
        ExprKind::Str(s) => Some(Json::String(s.clone())),
        ExprKind::Num(n) => Some(json!(n)),
        ExprKind::Bool(b) => Some(Json::Bool(*b)),
        ExprKind::Null => Some(Json::Null),
        ExprKind::Template(segs) => {
            let mut s = String::new();
            for seg in segs {
                match seg {
                    TemplateSeg::Str(x) => s.push_str(x),
                    _ => return None,
                }
            }
            Some(Json::String(s))
        }
        ExprKind::Array(items) => {
            let mut v = Vec::new();
            for i in items {
                match i {
                    ArrElem::Item(x) => v.push(const_eval(x)?),
                    _ => return None,
                }
            }
            Some(Json::Array(v))
        }
        ExprKind::Object(props) => {
            let mut m = Map::new();
            for p in props {
                match p {
                    ObjProp::KV(k, v) => {
                        m.insert(k.clone(), const_eval(v)?);
                    }
                    _ => return None,
                }
            }
            Some(Json::Object(m))
        }
        _ => None,
    }
}

impl<'a> Checker<'a> {
    // ---------------- statements ----------------
    pub fn check_stmts(&mut self, stmts: &[Stmt]) {
        self.hoist_local_functions(stmts);
        for s in stmts {
            if self.diverged && !self.warned_unreachable {
                self.warn(s.span, "unreachable code");
                self.warned_unreachable = true;
            }
            self.check_stmt(s);
        }
        self.warned_unreachable = false;
    }

    fn hoist_local_functions(&mut self, stmts: &[Stmt]) {
        for s in stmts {
            if let StmtKind::Func(f) = &s.kind {
                if f.ret.is_some() && self.fns.len() > 1 || (f.ret.is_some() && !self.fns.last().map_or(false, |c| c.top_level)) {
                    // declare with the signature only; body is checked in order
                    let sig = self.signature_only(f);
                    let name = f.name.clone().unwrap();
                    if self.scopes.last().map_or(false, |sc| sc.vars.contains_key(&name)) {
                        self.err(f.span, format!("'{}' is already declared in this scope", name));
                    } else {
                        self.declare_local(&name, sig, false, f.span, false);
                    }
                }
            }
        }
    }

    fn signature_only(&mut self, f: &FuncDecl) -> Ty {
        let names = self.new_tparam_names_pub(&f.tparams);
        let mut sc = Scope::default();
        for (tp, n) in f.tparams.iter().zip(names.iter()) {
            sc.types.insert(tp.name.clone(), n.clone());
        }
        self.scopes.push(sc);
        let mut tps = Vec::new();
        for (tp, n) in f.tparams.iter().zip(names.iter()) {
            let c = tp.constraint.as_ref().map(|c| self.resolve_type(c));
            if let Some(c) = &c {
                self.tcx.constraints.insert(n.clone(), c.clone());
            }
            tps.push(TParam { name: n.clone(), constraint: c });
        }
        let mut params = Vec::new();
        for p in &f.params {
            let t = match &p.ty {
                Some(t) => self.resolve_type(t),
                None => Ty::Any,
            };
            let elem = self.rest_elem(t, p.rest, f.span);
            let mut nm = Vec::new();
            pattern_names(&p.pattern, &mut nm);
            let name = nm.first().map(|x| x.0.clone()).unwrap_or_default();
            params.push(ParamTy { name: Rc::from(name.as_str()), ty: elem, optional: p.optional || p.default.is_some(), rest: p.rest });
        }
        let mut r = f.ret.as_ref().map(|t| self.resolve_type(t)).unwrap_or(Ty::Any);
        if f.is_async && !matches!(r, Ty::Promise(_) | Ty::Error | Ty::Any) {
            r = Ty::Error;
        }
        self.scopes.pop();
        Ty::Func(Rc::new(FnTy { tparams: tps, params, ret: r }))
    }

    pub fn new_tparam_names_pub(&mut self, tps: &[crate::ast::TypeParam]) -> Vec<Rc<str>> {
        tps.iter()
            .map(|tp| {
                let u = self.uid();
                Rc::from(format!("{}#{}", tp.name, u))
            })
            .collect()
    }

    pub fn check_block(&mut self, stmts: &[Stmt]) {
        self.push_scope();
        self.check_stmts(stmts);
        self.pop_scope();
    }

    fn check_branch(&mut self, s: &Stmt) {
        // a single-statement branch gets its own scope
        self.push_scope();
        self.check_stmt(s);
        self.pop_scope();
    }

    pub fn check_stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Var { kind, pattern, ty, init, .. } => {
                let bound = self.check_var_decl_types(*kind, pattern, ty.as_ref(), init.as_ref(), s.span);
                for (n, t, sp) in bound {
                    self.declare_local(&n, t, *kind == VarKind::Let, sp, true);
                }
            }
            StmtKind::Expr(e) => {
                let t = self.check_expr(e, None);
                if matches!(t, Ty::Promise(_)) && !matches!(e.kind, ExprKind::Assign { .. }) {
                    self.warn(e.span, "this Promise is not awaited; add 'await' (or assign it)");
                }
            }
            StmtKind::Block(b) => self.check_block(b),
            StmtKind::If(c, a, b) => self.check_if(c, a, b.as_deref()),
            StmtKind::While(c, body) => self.check_while(c, body, false),
            StmtKind::DoWhile(body, c) => self.check_while(c, body, true),
            StmtKind::For { init, cond, update, body } => self.check_for(init.as_deref(), cond.as_ref(), update.as_ref(), body),
            StmtKind::ForOf { kind, pattern, iter, body } => self.check_for_of(*kind, pattern, iter, body),
            StmtKind::Switch(d, cases) => self.check_switch(d, cases),
            StmtKind::Return(v) => self.check_return(v.as_ref(), s.span),
            StmtKind::Break => {
                if self.loop_depth == 0 && self.switch_depth == 0 {
                    self.err(s.span, "'break' is only allowed inside a loop or switch");
                }
                self.broke = true;
                self.diverged = true;
            }
            StmtKind::Continue => {
                if self.loop_depth == 0 {
                    self.err(s.span, "'continue' is only allowed inside a loop");
                }
                self.diverged = true;
            }
            StmtKind::Throw(e) => {
                let t = self.check_expr(e, None);
                let et = self.error_ty();
                if !self.tcx.assignable(&t, &et) {
                    let ts = self.show(&t);
                    self.err(e.span, format!("only Error objects can be thrown (an object with 'name' and 'message'), found '{}'", ts));
                }
                self.diverged = true;
            }
            StmtKind::Try { block, catch, finally } => self.check_try(block, catch.as_ref(), finally.as_ref()),
            StmtKind::Func(f) => {
                let name = f.name.clone().unwrap();
                let hoisted = self.scopes.last().map_or(false, |sc| sc.vars.contains_key(&name)) && f.ret.is_some();
                let saved_div = self.diverged;
                let t = self.check_function(f, None);
                self.diverged = saved_div;
                if !hoisted {
                    self.declare_local(&name, t, false, f.span, false);
                }
            }
            StmtKind::TypeAlias { .. } | StmtKind::Interface(_) => {
                self.err(s.span, "type declarations are only allowed at the top level of a module");
            }
            StmtKind::Import { .. } | StmtKind::ExportNames(_) | StmtKind::DeclareFunc { .. } | StmtKind::DeclareConst { .. } | StmtKind::Test { .. } => {
                self.err(s.span, "this declaration is only allowed at the top level");
            }
        }
    }

    fn check_cond(&mut self, c: &Expr) -> Narrow {
        let t = self.check_expr(c, None);
        // conditions of function/promise type are almost always bugs
        if matches!(t, Ty::Promise(_)) {
            self.warn(c.span, "condition is a Promise (missing 'await'?)");
        }
        self.narrow_of(c)
    }

    fn check_if(&mut self, c: &Expr, a: &Stmt, b: Option<&Stmt>) {
        let n = self.check_cond(c);
        let base = self.facts.clone();
        self.apply_facts(&n.t);
        self.check_branch(a);
        let then_facts = std::mem::replace(&mut self.facts, base.clone());
        let then_div = self.diverged;
        self.diverged = false;
        self.apply_facts(&n.f);
        if let Some(b) = b {
            self.check_branch(b);
        }
        let else_facts = std::mem::take(&mut self.facts);
        let else_div = self.diverged;
        match (then_div, else_div) {
            (true, true) => {
                self.facts = base;
                self.diverged = true;
            }
            (true, false) => {
                self.facts = else_facts;
                self.diverged = false;
            }
            (false, true) => {
                self.facts = then_facts;
                self.diverged = false;
            }
            (false, false) => {
                let mut merged = base.clone();
                let mut keys: HashSet<&String> = then_facts.keys().collect();
                keys.extend(else_facts.keys());
                for k in keys {
                    match (then_facts.get(k), else_facts.get(k)) {
                        (Some(x), Some(y)) => {
                            merged.insert(k.clone(), mk_union(vec![x.clone(), y.clone()]));
                        }
                        _ => {
                            merged.remove(k);
                        }
                    }
                }
                self.facts = merged;
                self.diverged = false;
            }
        }
    }

    fn invalidate_assigned(&mut self, names: &HashSet<String>) {
        // names are source names; drop facts of any local/top symbol with that name
        let mut keys: Vec<String> = Vec::new();
        for n in names {
            if let Some(l) = self.find_local(n) {
                keys.push(format!("@{}", l.id));
            }
            keys.push(format!("#{}:{}", self.cur, n));
        }
        for k in keys {
            self.invalidate(&k);
        }
    }

    fn check_while(&mut self, c: &Expr, body: &Stmt, is_do: bool) {
        let mut assigned = HashSet::new();
        collect_assigned_expr_pub(c, &mut assigned);
        collect_assigned_stmt_pub(body, &mut assigned);
        self.invalidate_assigned(&assigned);
        let saved_broke = self.broke;
        self.broke = false;
        self.loop_depth += 1;
        let always = matches!(c.kind, ExprKind::Bool(true));
        if is_do {
            self.check_branch(body);
            self.diverged = false;
            self.check_cond(c);
        } else {
            let n = self.check_cond(c);
            let base = self.facts.clone();
            self.apply_facts(&n.t);
            self.check_branch(body);
            self.facts = base;
        }
        self.loop_depth -= 1;
        let had_break = self.broke;
        self.broke = saved_broke;
        self.invalidate_assigned(&assigned);
        self.diverged = always && !had_break;
    }

    fn check_for(&mut self, init: Option<&Stmt>, cond: Option<&Expr>, update: Option<&Expr>, body: &Stmt) {
        self.push_scope();
        if let Some(i) = init {
            self.check_stmt(i);
        }
        let mut assigned = HashSet::new();
        if let Some(c) = cond {
            collect_assigned_expr_pub(c, &mut assigned);
        }
        if let Some(u) = update {
            collect_assigned_expr_pub(u, &mut assigned);
        }
        collect_assigned_stmt_pub(body, &mut assigned);
        self.invalidate_assigned(&assigned);
        let saved_broke = self.broke;
        self.broke = false;
        self.loop_depth += 1;
        let n = match cond {
            Some(c) => self.check_cond(c),
            None => Narrow::none(),
        };
        let base = self.facts.clone();
        self.apply_facts(&n.t);
        self.check_branch(body);
        self.diverged = false;
        if let Some(u) = update {
            self.check_expr(u, None);
        }
        self.facts = base;
        self.loop_depth -= 1;
        let had_break = self.broke;
        self.broke = saved_broke;
        self.invalidate_assigned(&assigned);
        let always = cond.is_none() || matches!(cond.map(|c| &c.kind), Some(ExprKind::Bool(true)));
        self.diverged = always && !had_break;
        self.pop_scope();
    }

    fn check_for_of(&mut self, kind: VarKind, pattern: &Pattern, iter: &Expr, body: &Stmt) {
        let it = self.check_expr(iter, None);
        let elem = match self.tcx.resolve(&it) {
            Ty::Array(e) => (*e).clone(),
            Ty::Tuple(v) => mk_union(v.iter().cloned().collect()),
            Ty::Str | Ty::LitStr(_) => Ty::Str,
            Ty::Any | Ty::Error => Ty::Any,
            other => {
                let ts = self.show(&other);
                self.err(iter.span, format!("type '{}' is not iterable; for-of works on arrays and strings (use Object.entries for objects)", ts));
                Ty::Error
            }
        };
        let mut assigned = HashSet::new();
        collect_assigned_stmt_pub(body, &mut assigned);
        self.invalidate_assigned(&assigned);
        self.push_scope();
        let mut out = Vec::new();
        self.bind_pattern(pattern, &elem, &mut out);
        for (n, t, sp) in out {
            self.declare_local(&n, t, kind == VarKind::Let, sp, true);
        }
        let saved_broke = self.broke;
        self.loop_depth += 1;
        let base = self.facts.clone();
        self.check_branch(body);
        self.facts = base;
        self.loop_depth -= 1;
        self.broke = saved_broke;
        self.invalidate_assigned(&assigned);
        self.diverged = false;
        self.pop_scope();
    }

    fn check_switch(&mut self, d: &Expr, cases: &[Case]) {
        let dt = self.check_expr(d, None);
        let base = self.facts.clone();
        let saved_broke = self.broke;
        self.broke = false;
        self.switch_depth += 1;
        let has_default = cases.iter().any(|c| c.test.is_none());
        let mut all_end_diverged = true;
        let mut seen_default_narrow: Vec<(String, Ty)> = Vec::new();
        let mut pending: Vec<Vec<(String, Ty)>> = Vec::new();
        let mut falls_through = false;
        let mut remaining_facts: Vec<(String, Ty)> = Vec::new();
        for c in cases {
            let mut label_facts: Vec<(String, Ty)> = Vec::new();
            if let Some(t) = &c.test {
                let tt = self.check_expr(t, Some(&dt));
                if !matches!(dt, Ty::Unknown | Ty::Any | Ty::Error) && !matches!(tt, Ty::Any | Ty::Error) {
                    let ok = members(&tt).iter().any(|m| members(&dt).iter().any(|x| self.tcx.assignable(m, x) || self.tcx.assignable(&widen(m), &widen(x))));
                    if !ok {
                        let (a, b) = (self.show(&dt), self.show(&tt));
                        self.err(t.span, format!("case type '{}' can never match the switch value of type '{}'", b, a));
                    }
                }
                // narrowing from case label
                let lit_expr = Expr { kind: ExprKind::Binary(BinOp::Eq, Box::new(d.clone()), Box::new(t.clone())), span: t.span };
                if matches!(t.kind, ExprKind::Str(_) | ExprKind::Num(_) | ExprKind::Bool(_)) {
                    let saved = std::mem::replace(&mut self.facts, base.clone());
                    self.apply_facts(&remaining_facts_unused());
                    let n = self.narrow_of(&lit_expr);
                    self.facts = saved;
                    label_facts = n.t.clone();
                    seen_default_narrow.extend(n.f);
                    let _ = &remaining_facts;
                }
                pending.push(label_facts);
            }
            if c.body.is_empty() && c.test.is_some() {
                // grouped labels: accumulate
                falls_through = true;
                continue;
            }
            // combine pending label facts (union per key present in all)
            let mut combined: Vec<(String, Ty)> = Vec::new();
            if !pending.is_empty() && !falls_through_with_unnarrowed(&pending) {
                let first = &pending[0];
                for (k, t) in first {
                    let mut tys = vec![t.clone()];
                    let mut in_all = true;
                    for other in &pending[1..] {
                        match other.iter().find(|(k2, _)| k2 == k) {
                            Some((_, t2)) => tys.push(t2.clone()),
                            None => in_all = false,
                        }
                    }
                    if in_all {
                        combined.push((k.clone(), mk_union(tys)));
                    }
                }
            }
            pending.clear();
            let _ = falls_through;
            falls_through = false;
            self.facts = base.clone();
            if c.test.is_none() {
                self.apply_facts(&seen_default_narrow);
            } else {
                self.apply_facts(&combined);
            }
            self.diverged = false;
            self.push_scope();
            self.check_stmts(&c.body);
            self.pop_scope();
            remaining_facts.clear();
            // a case that neither diverges nor breaks falls into the next one: that is allowed, but
            // for flow analysis it simply means the end is reachable.
            if !self.diverged {
                all_end_diverged = false;
            }
        }
        self.switch_depth -= 1;
        let had_break = self.broke;
        self.broke = saved_broke;
        self.facts = base;
        // exhaustive without default: remaining discriminant type is never
        let exhaustive = has_default || {
            let ms = members(&dt);
            !ms.is_empty() && ms.iter().all(|m| matches!(m, Ty::LitStr(_) | Ty::LitNum(_) | Ty::LitBool(_))) && {
                let mut covered = 0;
                for c in cases {
                    if let Some(t) = &c.test {
                        let tt = self.quiet_type(t);
                        if members(&dt).iter().any(|m| *m == tt) {
                            covered += 1;
                        }
                    }
                }
                covered >= ms.len()
            }
        };
        self.diverged = exhaustive && all_end_diverged && !had_break && !cases.is_empty();
        // clear any stale facts about the discriminant after the switch
        let mut assigned = HashSet::new();
        for c in cases {
            collect_assigned_stmts(&c.body, &mut assigned);
        }
        self.invalidate_assigned(&assigned);
    }

    fn check_return(&mut self, v: Option<&Expr>, span: Span) {
        let Some(ctx) = self.fns.last() else { return };
        if ctx.top_level {
            self.err(span, "'return' is not allowed at the top level of a module");
            return;
        }
        let expected = ctx.ret_expected.clone();
        let hint = ctx.ret_hint.clone();
        match v {
            Some(e) => {
                let h = expected.clone().or(hint);
                let t = self.check_expr(e, h.as_ref());
                if let Some(exp) = &expected {
                    self.expect(&t, exp, e.span, "return value");
                }
                if let Some(c) = self.fns.last_mut() {
                    c.returns.push(t);
                }
            }
            None => {
                if let Some(exp) = &expected {
                    if !matches!(exp, Ty::Void | Ty::Any | Ty::Unknown | Ty::Error) && !has_void(exp) {
                        let ts = self.show(exp);
                        self.err(span, format!("this function must return a value of type '{}'", ts));
                    }
                }
                if let Some(c) = self.fns.last_mut() {
                    c.returns.push(Ty::Void);
                }
            }
        }
        self.diverged = true;
    }

    fn check_try(&mut self, block: &[Stmt], catch: Option<&(Option<Pattern>, Vec<Stmt>)>, finally: Option<&Vec<Stmt>>) {
        let base = self.facts.clone();
        let mut assigned = HashSet::new();
        collect_assigned_stmts(block, &mut assigned);
        self.check_block(block);
        let try_div = self.diverged;
        self.facts = base.clone();
        self.invalidate_assigned(&assigned);
        let mut catch_div = true;
        if let Some((binding, cbody)) = catch {
            self.diverged = false;
            self.push_scope();
            if let Some(p) = binding {
                let et = self.error_ty();
                let mut out = Vec::new();
                self.bind_pattern(p, &et, &mut out);
                for (n, t, sp) in out {
                    self.declare_local(&n, t, true, sp, false);
                }
            }
            self.check_stmts(cbody);
            self.pop_scope();
            catch_div = self.diverged;
            collect_assigned_stmts(cbody, &mut assigned);
        } else {
            catch_div = false;
        }
        self.facts = base;
        self.invalidate_assigned(&assigned);
        let mut div = try_div && catch_div;
        if let Some(f) = finally {
            self.diverged = false;
            self.check_block(f);
            if self.diverged {
                div = true;
            }
            let mut a2 = HashSet::new();
            collect_assigned_stmts(f, &mut a2);
            self.invalidate_assigned(&a2);
        }
        self.diverged = div;
    }

    // ---------------- declarations ----------------
    /// Check a variable declaration and return the bound names with their types (without declaring them).
    pub fn check_var_decl_types(&mut self, kind: VarKind, pattern: &Pattern, ty: Option<&TypeExpr>, init: Option<&Expr>, span: Span) -> Vec<(String, Ty, Span)> {
        let declared = ty.map(|t| self.resolve_type(t));
        let vty = match (init, &declared) {
            (Some(i), Some(d)) => {
                let it = self.check_expr(i, Some(d));
                self.expect(&it, d, i.span, "initializer");
                d.clone()
            }
            (Some(i), None) => {
                let it = self.check_expr(i, None);
                match &it {
                    Ty::Array(e) if matches!(**e, Ty::Never) => {
                        if self.is_strict {
                            self.err(i.span, "cannot infer the element type of an empty array; add a type annotation (e.g. `let x: string[] = []`)");
                            Ty::Error
                        } else {
                            Ty::Array(Rc::new(Ty::Any))
                        }
                    }
                    Ty::Null if kind == VarKind::Let => {
                        self.err(i.span, "cannot infer a type from 'null'; add a type annotation (e.g. `let x: string | null = null`)");
                        Ty::Error
                    }
                    _ => {
                        if kind == VarKind::Const {
                            it
                        } else {
                            widen(&it)
                        }
                    }
                }
            }
            (None, Some(d)) => d.clone(),
            (None, None) => {
                self.err(span, "a variable declaration needs a type annotation or an initializer");
                Ty::Error
            }
        };
        let mut out = Vec::new();
        self.bind_pattern(pattern, &vty, &mut out);
        out
    }

    pub fn bind_pattern(&mut self, p: &Pattern, ty: &Ty, out: &mut Vec<(String, Ty, Span)>) {
        match p {
            Pattern::Ident(n, sp) => out.push((n.clone(), ty.clone(), *sp)),
            Pattern::Object { props, rest, span } => {
                let rt = self.tcx.resolve(ty);
                let mut used_keys = Vec::new();
                for pr in props {
                    used_keys.push(pr.key.clone());
                    let mut pt = match self.get_member(&rt, &pr.key) {
                        Ok(t) => t,
                        Err(m) => {
                            self.err(*span, m);
                            Ty::Error
                        }
                    };
                    if let Some(d) = &pr.default {
                        let dt = self.check_expr(d, Some(&pt));
                        pt = mk_union(vec![remove_null(&pt), widen(&dt)]);
                        let _ = dt;
                    }
                    self.bind_pattern(&pr.target, &pt, out);
                }
                if let Some(r) = rest {
                    let mut o = ObjTy::default();
                    if let Ty::Obj(src) = &rt {
                        for (k, pp) in src.props.iter() {
                            if !used_keys.contains(k) {
                                o.props.insert(k.clone(), pp.clone());
                            }
                        }
                        o.index = src.index.clone();
                    }
                    out.push((r.clone(), Ty::Obj(Rc::new(o)), *span));
                }
            }
            Pattern::Array { elems, rest, span } => {
                let rt = self.tcx.resolve(ty);
                for (i, el) in elems.iter().enumerate() {
                    let Some(el) = el else { continue };
                    let mut et = match &rt {
                        Ty::Array(e) => (**e).clone(),
                        Ty::Tuple(v) => match v.get(i) {
                            Some(t) => t.clone(),
                            None => {
                                self.err(*span, format!("tuple of length {} has no element at index {}", v.len(), i));
                                Ty::Error
                            }
                        },
                        Ty::Any | Ty::Error => Ty::Any,
                        other => {
                            let ts = self.show(other);
                            self.err(*span, format!("type '{}' cannot be destructured as an array", ts));
                            Ty::Error
                        }
                    };
                    if let Some(d) = &el.default {
                        let dt = self.check_expr(d, Some(&et));
                        et = mk_union(vec![remove_null(&et), widen(&dt)]);
                    }
                    self.bind_pattern(&el.target, &et, out);
                }
                if let Some(r) = rest {
                    let rt2 = match &rt {
                        Ty::Array(e) => Ty::Array(e.clone()),
                        Ty::Tuple(v) => Ty::Array(Rc::new(mk_union(v.iter().skip(elems.len()).cloned().collect()))),
                        _ => Ty::Array(Rc::new(Ty::Any)),
                    };
                    out.push((r.clone(), rt2, *span));
                }
            }
        }
    }

    // ---------------- functions ----------------
    pub fn check_function(&mut self, f: &FuncDecl, ctx: Option<&FnTy>) -> Ty {
        let names = self.new_tparam_names_pub(&f.tparams);
        let mut tscope = Scope::default();
        for (tp, n) in f.tparams.iter().zip(names.iter()) {
            tscope.types.insert(tp.name.clone(), n.clone());
        }
        self.scopes.push(tscope);
        let mut tps = Vec::new();
        for (tp, n) in f.tparams.iter().zip(names.iter()) {
            let c = tp.constraint.as_ref().map(|c| self.resolve_type(c));
            if let Some(c) = &c {
                self.tcx.constraints.insert(n.clone(), c.clone());
            }
            tps.push(TParam { name: n.clone(), constraint: c });
        }
        // parameters
        let mut ptys: Vec<ParamTy> = Vec::new();
        let is_std = self.loaded.modules[self.cur].is_std;
        for (i, p) in f.params.iter().enumerate() {
            let ann = p.ty.as_ref().map(|t| (self.resolve_type(t), crate::checker::decl_span(t)));
            let elem = match ann {
                Some((t, sp)) => self.rest_elem(t, p.rest, sp),
                None => match ctx.and_then(|c| c.params.get(i)) {
                    Some(cp) if !matches!(cp.ty, Ty::Unknown) => cp.ty.clone(),
                    Some(_) | None => {
                        if self.is_strict && !is_std {
                            let mut nm = Vec::new();
                            pattern_names(&p.pattern, &mut nm);
                            let n = nm.first().map(|x| x.0.clone()).unwrap_or_default();
                            self.err(f.span, format!("parameter '{}' needs a type annotation (strict_types is enabled)", n));
                        }
                        Ty::Any
                    }
                },
            };
            let mut nm = Vec::new();
            pattern_names(&p.pattern, &mut nm);
            let name = nm.first().map(|x| x.0.clone()).unwrap_or_else(|| format!("arg{}", i));
            ptys.push(ParamTy { name: Rc::from(name.as_str()), ty: elem, optional: p.optional || p.default.is_some(), rest: p.rest });
        }
        // return type
        let ann_ret = f.ret.as_ref().map(|t| (self.resolve_type(t), crate::checker::decl_span(t)));
        let (declared_inner, declared_full): (Option<Ty>, Option<Ty>) = match ann_ret {
            Some((r, sp)) => {
                if f.is_async {
                    match r {
                        Ty::Promise(i) => (Some((*i).clone()), Some(Ty::Promise(i))),
                        Ty::Error => (Some(Ty::Error), Some(Ty::Error)),
                        other => {
                            let ts = self.show(&other);
                            self.err(sp.max_span(f.span), format!("the return type of an async function must be Promise<T>, found '{}'", ts));
                            (Some(Ty::Error), Some(Ty::Error))
                        }
                    }
                } else {
                    (Some(r.clone()), Some(r))
                }
            }
            None => (None, None),
        };
        let hint = ctx.and_then(|c| match (&c.ret, f.is_async) {
            (Ty::Promise(i), true) => Some((**i).clone()),
            (r, false) => Some(r.clone()),
            _ => None,
        });
        self.fns.push(FnCtx { ret_expected: declared_inner.clone(), ret_hint: hint, returns: vec![], is_async: f.is_async, top_level: false });
        // facts: only stable const bindings survive into a function body
        let outer_facts = std::mem::take(&mut self.facts);
        for (k, v) in outer_facts.iter() {
            if let Some(rest) = k.strip_prefix('@') {
                if !rest.contains('.') {
                    if let Ok(id) = rest.parse::<u32>() {
                        if self.local_mut.get(&id) == Some(&false) {
                            self.facts.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
        }
        let saved = (self.diverged, self.loop_depth, self.switch_depth, self.broke, self.warned_unreachable);
        self.diverged = false;
        self.loop_depth = 0;
        self.switch_depth = 0;
        self.broke = false;
        self.warned_unreachable = false;
        self.push_scope();
        for (p, pt) in f.params.iter().zip(ptys.iter()) {
            let bind_ty = if p.rest { Ty::Array(Rc::new(pt.ty.clone())) } else { pt.ty.clone() };
            let bind_ty = if p.optional && p.default.is_none() { mk_union(vec![bind_ty, Ty::Null]) } else { bind_ty };
            if let Some(d) = &p.default {
                let dt = self.check_expr(d, Some(&pt.ty));
                self.expect(&dt, &pt.ty, d.span, "default value");
            }
            let mut out = Vec::new();
            self.bind_pattern(&p.pattern, &bind_ty, &mut out);
            for (n, t, sp) in out {
                self.declare_local(&n, t, true, sp, false);
            }
        }
        if let Some(full) = &declared_full {
            // publish the signature early so recursive calls resolve
            let sig = Ty::Func(Rc::new(FnTy { tparams: tps.clone(), params: ptys.clone(), ret: full.clone() }));
            self.fn_types.entry(f as *const FuncDecl).or_insert(sig);
        }
        self.check_stmts(&f.body);
        let falls_off = !self.diverged;
        self.pop_scope();
        let fctx = self.fns.pop().unwrap();
        self.facts = outer_facts;
        self.diverged = saved.0;
        self.loop_depth = saved.1;
        self.switch_depth = saved.2;
        self.broke = saved.3;
        self.warned_unreachable = saved.4;
        let ret = match (declared_full, declared_inner) {
            (Some(full), Some(inner)) => {
                if falls_off && !matches!(inner, Ty::Void | Ty::Any | Ty::Unknown | Ty::Error) && !has_void(&inner) && !f.is_arrow_expr_body() {
                    let ts = self.show(&inner);
                    self.err(f.span, format!("function lacks an ending return statement (return type is '{}')", ts));
                }
                full
            }
            _ => {
                let has_value_return = fctx.returns.iter().any(|t| !matches!(t, Ty::Void));
                if has_value_return && falls_off {
                    self.err(f.span, "not all code paths return a value; add a return statement or an explicit return type");
                }
                let inner = if fctx.returns.is_empty() {
                    Ty::Void
                } else {
                    let vals: Vec<Ty> = fctx.returns.iter().map(widen).collect();
                    mk_union(vals)
                };
                if f.is_async {
                    mk_promise(inner)
                } else {
                    inner
                }
            }
        };
        self.scopes.pop();
        Ty::Func(Rc::new(FnTy { tparams: tps, params: ptys, ret }))
    }

    // ---------------- decorators ----------------
    pub fn check_decorators(&mut self, f: &Rc<FuncDecl>) {
        let key = Rc::as_ptr(f);
        let Some(Ty::Func(fty)) = self.fn_types.get(&key).cloned() else { return };
        if f.decorators.len() > 1 {
            self.err(f.decorators[1].span, "a function can have only one decorator");
        }
        let d = &f.decorators[0];
        let mut args: Vec<Json> = Vec::new();
        for a in &d.args {
            match const_eval(a) {
                Some(j) => args.push(j),
                None => {
                    self.err(a.span, "decorator arguments must be constant values (strings, numbers, booleans, arrays or objects of constants)");
                    return;
                }
            }
        }
        let inner_ret = match &fty.ret {
            Ty::Promise(i) => (**i).clone(),
            other => other.clone(),
        };
        let mut meta = FuncMeta { decorator_opts: vec![(d.name.clone(), args.clone())], ..Default::default() };
        match d.name.as_str() {
            "tool" => self.decorate_tool(f, &fty, &inner_ret, &args, d.span, &mut meta),
            "resource" => self.decorate_resource(f, &fty, &inner_ret, &args, d.span, &mut meta),
            "prompt" => self.decorate_prompt(f, &fty, &args, &inner_ret, d.span, &mut meta),
            other => {
                self.err(d.span, format!("unknown decorator '@{}'; expected @tool, @resource or @prompt", other));
            }
        }
        *f.meta.borrow_mut() = meta;
    }

    fn check_opts(&mut self, span: Span, opts: &Json, allowed: &[&str]) {
        if let Json::Object(m) = opts {
            for (k, v) in m {
                if !allowed.contains(&k.as_str()) {
                    self.err(span, format!("unknown decorator option '{}'; allowed: {}", k, allowed.join(", ")));
                    continue;
                }
                let ok = match k.as_str() {
                    "name" | "title" | "description" | "mimeType" => v.is_string(),
                    "params" => v.as_object().map_or(false, |o| o.values().all(|x| x.is_string())),
                    "readOnly" | "destructive" | "idempotent" | "openWorld" => v.is_boolean(),
                    _ => true,
                };
                if !ok {
                    self.err(span, format!("decorator option '{}' has the wrong type", k));
                }
            }
        }
    }

    fn param_list(&mut self, f: &FuncDecl, fty: &FnTy, span: Span) -> Vec<(String, Ty, bool, Option<Json>)> {
        let mut v = Vec::new();
        for (p, pt) in f.params.iter().zip(fty.params.iter()) {
            match &p.pattern {
                Pattern::Ident(n, _) => {
                    if p.rest {
                        self.err(span, "rest parameters are not allowed on decorated functions");
                    }
                    let required = !(p.optional || p.default.is_some());
                    let default = p.default.as_ref().and_then(const_eval);
                    v.push((n.clone(), pt.ty.clone(), required, default));
                }
                _ => self.err(span, "destructured parameters are not allowed on decorated functions; use a single named parameter"),
            }
        }
        v
    }

    fn valid_name(n: &str) -> bool {
        !n.is_empty() && n.len() <= 128 && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    }

    fn decorate_tool(&mut self, f: &FuncDecl, fty: &FnTy, ret: &Ty, args: &[Json], span: Span, meta: &mut FuncMeta) {
        let opts: Json = match args.first() {
            Some(Json::String(s)) => json!({"description": s}),
            Some(o @ Json::Object(_)) => o.clone(),
            None => json!({}),
            _ => {
                self.err(span, "@tool expects a description string or an options object");
                json!({})
            }
        };
        if args.len() > 1 {
            self.err(span, "@tool takes at most one argument");
        }
        self.check_opts(span, &opts, &["name", "title", "description", "params", "readOnly", "destructive", "idempotent", "openWorld"]);
        let name = opts.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()).or_else(|| f.name.clone()).unwrap_or_default();
        if !Self::valid_name(&name) {
            self.err(span, format!("invalid tool name '{}': use 1-128 letters, digits, '_', '-' or '.'", name));
        }
        if let Some(prev) = self.tool_names.get(&format!("tool:{}", name)) {
            let _ = prev;
            self.err(span, format!("duplicate tool name '{}'", name));
        }
        self.tool_names.insert(format!("tool:{}", name), span);
        if opts.get("description").is_none() {
            self.warn(span, format!("tool '{}' has no description; models rely on descriptions to choose tools", name));
        }
        let params = self.param_list(f, fty, span);
        let descs = opts.get("params").and_then(|p| p.as_object()).cloned().unwrap_or_default();
        for k in descs.keys() {
            if !params.iter().any(|(n, ..)| n == k) {
                self.err(span, format!("'params' documents unknown parameter '{}'", k));
            }
        }
        let mut props = Map::new();
        let mut required = Vec::new();
        let mut pnames = Vec::new();
        let mut preq = Vec::new();
        for (n, t, req, default) in &params {
            let mut s = match crate::schema::ty_schema(&self.tcx, t) {
                Ok(s) => s,
                Err(m) => {
                    self.err(span, format!("parameter '{}' cannot be used in a tool: {}", n, m));
                    json!({})
                }
            };
            if let (Json::Object(m), Some(d)) = (&mut s, descs.get(n)) {
                m.insert("description".into(), d.clone());
            }
            if let (Json::Object(m), Some(d)) = (&mut s, default) {
                m.insert("default".into(), d.clone());
            }
            props.insert(n.clone(), s);
            if *req {
                required.push(Json::String(n.clone()));
            }
            pnames.push(n.clone());
            preq.push(*req);
        }
        let mut input = Map::new();
        input.insert("type".into(), json!("object"));
        input.insert("properties".into(), Json::Object(props));
        if !required.is_empty() {
            input.insert("required".into(), Json::Array(required));
        }
        meta.input_schema = Some(Json::Object(input));
        meta.param_names = pnames;
        meta.param_required = preq;
        // result kind
        let tool_result_id = self.mcp_types.get("ToolResult").copied();
        let kind = match ret {
            Ty::Ref(id, _) if Some(*id) == tool_result_id => "passthrough",
            Ty::Str | Ty::LitStr(_) => "text",
            Ty::Void => "void",
            Ty::Obj(_) | Ty::Ref(..) => "structured",
            Ty::Error | Ty::Any | Ty::Unknown => "json",
            Ty::Func(_) | Ty::Promise(_) => {
                self.err(span, "a tool cannot return a function or nested Promise");
                "json"
            }
            _ => "json",
        };
        meta.result_kind = kind.to_string();
        if kind == "structured" {
            match crate::schema::ty_schema(&self.tcx, ret) {
                Ok(s) => meta.output_schema = Some(s),
                Err(m) => self.err(span, format!("return type cannot be described as an output schema: {}", m)),
            }
        }
    }

    fn decorate_resource(&mut self, f: &FuncDecl, fty: &FnTy, ret: &Ty, args: &[Json], span: Span, meta: &mut FuncMeta) {
        let Some(Json::String(uri)) = args.first() else {
            self.err(span, "@resource requires a URI string as its first argument, e.g. @resource(\"config://app\")");
            return;
        };
        if args.len() > 2 {
            self.err(span, "@resource takes at most two arguments (uri, options)");
        }
        let valid_scheme = uri.find(':').map_or(false, |i| i > 0 && uri[..i].chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '.' || c == '-') && uri.chars().next().map_or(false, |c| c.is_ascii_alphabetic()));
        if !valid_scheme {
            self.err(span, format!("invalid resource URI '{}': expected a scheme such as 'file://' or 'config://'", uri));
        }
        if let Some(o) = args.get(1) {
            if !o.is_object() {
                self.err(span, "the second argument of @resource must be an options object");
            } else {
                self.check_opts(span, o, &["name", "title", "description", "mimeType"]);
            }
        }
        if let Some(prev) = self.tool_names.get(&format!("resource:{}", uri)) {
            let _ = prev;
            self.err(span, format!("duplicate resource URI '{}'", uri));
        }
        self.tool_names.insert(format!("resource:{}", uri), span);
        // template variables
        let mut vars = Vec::new();
        let mut rest = uri.as_str();
        while let Some(i) = rest.find('{') {
            if let Some(j) = rest[i..].find('}') {
                vars.push(rest[i + 1..i + j].to_string());
                rest = &rest[i + j + 1..];
            } else {
                self.err(span, "unterminated '{' in resource URI template");
                break;
            }
        }
        let params = self.param_list(f, fty, span);
        for (n, t, _, _) in &params {
            if !matches!(t, Ty::Str) {
                self.err(span, format!("resource parameter '{}' must be a string", n));
            }
            if !vars.contains(n) {
                self.err(span, format!("parameter '{}' does not appear in the URI template '{}'", n, uri));
            }
        }
        for v in &vars {
            if !params.iter().any(|(n, ..)| n == v) {
                self.err(span, format!("URI template variable '{{{}}}' has no matching function parameter", v));
            }
        }
        let rc_id = self.mcp_types.get("ResourceContent").copied();
        let kind = match ret {
            Ty::Str | Ty::LitStr(_) => "string",
            Ty::Ref(id, _) if Some(*id) == rc_id => "content",
            Ty::Array(e) if matches!(&**e, Ty::Ref(id, _) if Some(*id) == rc_id) => "contents",
            Ty::Error => "string",
            _ => {
                let ts = self.show(ret);
                self.err(span, format!("a resource must return string, ResourceContent or ResourceContent[] (from std/mcp), found '{}'", ts));
                "string"
            }
        };
        meta.result_kind = kind.to_string();
        meta.param_names = params.iter().map(|p| p.0.clone()).collect();
        meta.param_required = params.iter().map(|p| p.2).collect();
    }

    fn decorate_prompt(&mut self, f: &FuncDecl, fty: &FnTy, args: &[Json], ret: &Ty, span: Span, meta: &mut FuncMeta) {
        let opts: Json = match args.first() {
            Some(Json::String(s)) => json!({"description": s}),
            Some(o @ Json::Object(_)) => o.clone(),
            None => json!({}),
            _ => {
                self.err(span, "@prompt expects a description string or an options object");
                json!({})
            }
        };
        if args.len() > 1 {
            self.err(span, "@prompt takes at most one argument");
        }
        self.check_opts(span, &opts, &["name", "title", "description", "params"]);
        let name = opts.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()).or_else(|| f.name.clone()).unwrap_or_default();
        if !Self::valid_name(&name) {
            self.err(span, format!("invalid prompt name '{}'", name));
        }
        if self.tool_names.contains_key(&format!("prompt:{}", name)) {
            self.err(span, format!("duplicate prompt name '{}'", name));
        }
        self.tool_names.insert(format!("prompt:{}", name), span);
        let params = self.param_list(f, fty, span);
        for (n, t, _, _) in &params {
            let ok = members(t).iter().all(|m| matches!(m, Ty::Str | Ty::Null | Ty::LitStr(_)));
            if !ok {
                self.err(span, format!("prompt argument '{}' must be a string", n));
            }
        }
        let pm_id = self.mcp_types.get("PromptMessage").copied();
        let kind = match ret {
            Ty::Str | Ty::LitStr(_) | Ty::Error => "string",
            Ty::Array(e) if matches!(&**e, Ty::Ref(id, _) if Some(*id) == pm_id) => "messages",
            _ => {
                let ts = self.show(ret);
                self.err(span, format!("a prompt must return string or PromptMessage[] (from std/mcp), found '{}'", ts));
                "string"
            }
        };
        meta.result_kind = kind.to_string();
        meta.param_names = params.iter().map(|p| p.0.clone()).collect();
        meta.param_required = params.iter().map(|p| p.2 && !has_null(&p.1)).collect();
    }
}

fn has_void(t: &Ty) -> bool {
    members(t).iter().any(|m| matches!(m, Ty::Void))
}

fn remaining_facts_unused() -> Vec<(String, Ty)> {
    vec![]
}

fn falls_through_with_unnarrowed(pending: &[Vec<(String, Ty)>]) -> bool {
    pending.iter().any(|p| p.is_empty())
}

pub fn collect_assigned_expr_pub(e: &Expr, out: &mut HashSet<String>) {
    collect_assigned_expr(e, out)
}
pub fn collect_assigned_stmt_pub(s: &Stmt, out: &mut HashSet<String>) {
    collect_assigned_stmt(s, out)
}

trait SpanExt {
    fn max_span(self, other: Span) -> Span;
}
impl SpanExt for Span {
    fn max_span(self, other: Span) -> Span {
        if self.line == 0 {
            other
        } else {
            self
        }
    }
}

trait FuncDeclExt {
    fn is_arrow_expr_body(&self) -> bool;
}
impl FuncDeclExt for FuncDecl {
    fn is_arrow_expr_body(&self) -> bool {
        false
    }
}
