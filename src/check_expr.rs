use crate::ast::*;
use crate::checker::*;
use crate::lexer::Span;
use crate::types::*;
use std::collections::HashMap;
use std::rc::Rc;

pub struct Narrow {
    pub t: Vec<(String, Ty)>,
    pub f: Vec<(String, Ty)>,
}

impl Narrow {
    pub fn none() -> Narrow {
        Narrow { t: vec![], f: vec![] }
    }
    pub fn swap(self) -> Narrow {
        Narrow { t: self.f, f: self.t }
    }
}

fn is_any(t: &Ty) -> bool {
    matches!(t, Ty::Any | Ty::Error)
}

fn all_members(t: &Ty, f: impl Fn(&Ty) -> bool) -> bool {
    members(t).iter().all(|m| f(m))
}

fn numberish(t: &Ty) -> bool {
    all_members(t, |m| matches!(m, Ty::Number | Ty::LitNum(_) | Ty::Any | Ty::Error | Ty::Never))
}

fn stringy(t: &Ty) -> bool {
    all_members(t, |m| matches!(m, Ty::Str | Ty::LitStr(_) | Ty::Any | Ty::Error | Ty::Never))
}

fn stringifiable(t: &Ty) -> bool {
    all_members(t, |m| matches!(m, Ty::Number | Ty::Str | Ty::Bool | Ty::Null | Ty::LitStr(_) | Ty::LitNum(_) | Ty::LitBool(_) | Ty::Any | Ty::Error | Ty::Never))
}

pub fn has_literal(t: &Ty) -> bool {
    members(t).iter().any(|m| matches!(m, Ty::LitStr(_) | Ty::LitNum(_) | Ty::LitBool(_)))
}

pub fn truthy_part(t: &Ty) -> Ty {
    mk_union(
        members(t)
            .into_iter()
            .filter_map(|m| match m {
                Ty::Null | Ty::Void | Ty::LitBool(false) => None,
                Ty::LitNum(n) if n == 0.0 => None,
                Ty::LitStr(s) if s.is_empty() => None,
                Ty::Bool => Some(Ty::LitBool(true)),
                other => Some(other),
            })
            .collect(),
    )
}

pub fn falsy_part(t: &Ty) -> Ty {
    mk_union(
        members(t)
            .into_iter()
            .filter_map(|m| match m {
                Ty::Null | Ty::Void | Ty::LitBool(false) | Ty::Str | Ty::Number | Ty::Any | Ty::Unknown | Ty::Error | Ty::Param(_) => Some(m),
                Ty::Bool => Some(Ty::LitBool(false)),
                Ty::LitNum(n) if n == 0.0 => Some(m),
                Ty::LitStr(ref s) if s.is_empty() => Some(m),
                _ => None,
            })
            .collect(),
    )
}

impl<'a> Checker<'a> {
    // ---------------- helpers ----------------
    pub fn expect(&mut self, src: &Ty, dst: &Ty, span: Span, what: &str) -> bool {
        if self.tcx.assignable(src, dst) {
            return true;
        }
        let (a, b) = (self.show(src), self.show(dst));
        self.err(span, format!("{}: type '{}' is not assignable to type '{}'", what, a, b));
        false
    }

    pub fn error_ty(&mut self) -> Ty {
        match self.mods[self.prelude_mod].exp_types.get("Error") {
            Some(TypeEntry::Iface(id)) => Ty::Ref(*id, Rc::new(vec![])),
            _ => Ty::Any,
        }
    }

    pub fn quiet_type(&mut self, e: &Expr) -> Ty {
        self.quiet += 1;
        let saved_chain = self.chain_null;
        let t = self.check_expr(e, None);
        self.chain_null = saved_chain;
        self.quiet -= 1;
        t
    }

    pub fn with_facts<T>(&mut self, add: &[(String, Ty)], f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = self.facts.clone();
        self.apply_facts(add);
        let r = f(self);
        self.facts = saved;
        r
    }

    pub fn apply_facts(&mut self, add: &[(String, Ty)]) {
        for (k, t) in add {
            self.facts.insert(k.clone(), t.clone());
        }
    }

    pub fn invalidate(&mut self, key: &str) {
        let prefix = format!("{}.", key);
        self.facts.retain(|k, _| k != key && !k.starts_with(&prefix));
    }

    pub fn ref_key(&mut self, e: &Expr) -> Option<String> {
        match &e.kind {
            ExprKind::Ident(n) => {
                if let Some(l) = self.find_local(n) {
                    Some(format!("@{}", l.id))
                } else if self.mods[self.cur].vals.get(n).map_or(false, |s| !matches!(*s.ty.borrow(), TopTy::Namespace(_))) {
                    Some(format!("#{}:{}", self.cur, n))
                } else {
                    None
                }
            }
            ExprKind::Member { obj, name, optional: false } => self.ref_key(obj).map(|k| format!("{}.{}", k, name)),
            _ => None,
        }
    }

    // ---------------- member lookup ----------------
    pub fn get_member(&mut self, base: &Ty, name: &str) -> Result<Ty, String> {
        match base {
            Ty::Any | Ty::Error => Ok(Ty::Any),
            Ty::Unknown => Err(format!("property '{}' does not exist on type 'unknown' (narrow it first)", name)),
            Ty::Null => Err(format!("cannot read property '{}' of null", name)),
            Ty::Never => Ok(Ty::Never),
            Ty::Union(u) => {
                if u.iter().any(|m| matches!(m, Ty::Null)) {
                    return Err(format!("object is possibly 'null' when reading '{}' (use '?.' or check for null)", name));
                }
                let mut out = Vec::new();
                for m in u.iter() {
                    match self.get_member(m, name) {
                        Ok(t) => out.push(t),
                        Err(_) => {
                            let ts = self.show(base);
                            return Err(format!("property '{}' does not exist on type '{}'", name, ts));
                        }
                    }
                }
                Ok(mk_union(out))
            }
            Ty::Obj(o) => {
                if let Some(p) = o.props.get(name) {
                    return Ok(if p.optional { mk_union(vec![p.ty.clone(), Ty::Null]) } else { p.ty.clone() });
                }
                if let Some(i) = &o.index {
                    return Ok(i.clone());
                }
                let ts = self.show(base);
                Err(format!("property '{}' does not exist on type '{}'", name, ts))
            }
            Ty::Ref(id, args) => {
                self.ensure_iface(*id);
                let o = self.tcx.expand(*id, args);
                self.get_member(&Ty::Obj(o), name).map_err(|_| {
                    let ts = self.tcx.show(base);
                    format!("property '{}' does not exist on type '{}'", name, ts)
                })
            }
            Ty::Array(e) => self.builtin_member("array", vec![(**e).clone()], name, base),
            Ty::Tuple(v) => {
                let el = mk_union(v.iter().map(widen).collect());
                self.builtin_member("array", vec![el], name, base)
            }
            Ty::Str | Ty::LitStr(_) => self.builtin_member("string", vec![], name, base),
            Ty::Number | Ty::LitNum(_) => self.builtin_member("number", vec![], name, base),
            Ty::Bool | Ty::LitBool(_) => self.builtin_member("boolean", vec![], name, base),
            Ty::Param(p) => match self.tcx.constraints.get(p).cloned() {
                Some(c) => self.get_member(&c, name),
                None => Err(format!("property '{}' does not exist on type '{}'", name, self.show(base))),
            },
            Ty::Func(_) | Ty::Promise(_) | Ty::Void => Err(format!("property '{}' does not exist on type '{}'", name, self.show(base))),
        }
    }

    fn builtin_member(&mut self, which: &'static str, args: Vec<Ty>, name: &str, base: &Ty) -> Result<Ty, String> {
        let Some(&id) = self.builtin.get(which) else {
            return Err("builtin type information is unavailable".into());
        };
        self.ensure_iface(id);
        let o = self.tcx.expand(id, &args);
        match o.props.get(name) {
            Some(p) => Ok(p.ty.clone()),
            None => Err(format!("property '{}' does not exist on type '{}'", name, self.show(base))),
        }
    }

    // ---------------- expressions ----------------
    pub fn exp_fn(&self, exp: Option<&Ty>) -> Option<Rc<FnTy>> {
        let t = exp?;
        let ms: Vec<Ty> = members(t).into_iter().filter(|m| !matches!(m, Ty::Null)).collect();
        let fns: Vec<&Ty> = ms.iter().filter(|m| matches!(m, Ty::Func(_))).collect();
        if fns.len() == 1 {
            if let Ty::Func(f) = fns[0] {
                return Some(f.clone());
            }
        }
        None
    }

    pub fn prop_expected(&self, exp: Option<&Ty>, key: &str) -> Option<Ty> {
        let t = exp?;
        let mut out = Vec::new();
        for m in members(t) {
            match self.tcx.resolve(&m) {
                Ty::Obj(o) => {
                    if let Some(p) = o.props.get(key) {
                        out.push(p.ty.clone());
                    } else if let Some(i) = &o.index {
                        out.push(i.clone());
                    }
                }
                _ => {}
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(mk_union(out))
        }
    }

    pub fn check_expr(&mut self, e: &Expr, exp: Option<&Ty>) -> Ty {
        let span = e.span;
        match &e.kind {
            ExprKind::Num(n) => Ty::LitNum(*n),
            ExprKind::Str(s) => Ty::LitStr(Rc::from(s.as_str())),
            ExprKind::Bool(b) => Ty::LitBool(*b),
            ExprKind::Null => Ty::Null,
            ExprKind::Template(segs) => {
                for s in segs {
                    if let TemplateSeg::Expr(x) = s {
                        let t = self.check_expr(x, None);
                        if !stringifiable(&t) {
                            let ts = self.show(&t);
                            self.err(x.span, format!("cannot interpolate a value of type '{}' in a template string; convert it explicitly (e.g. JSON.stringify)", ts));
                        }
                    }
                }
                Ty::Str
            }
            ExprKind::Ident(n) => self.check_ident(n, span),
            ExprKind::Array(items) => self.check_array(items, exp, span),
            ExprKind::Object(props) => self.check_object(props, exp, span),
            ExprKind::Func(f) => {
                let ctx = self.exp_fn(exp);
                self.check_function(f, ctx.as_deref())
            }
            ExprKind::Unary(op, x) => {
                let t = self.check_expr(x, None);
                match op {
                    UnOp::Not => Ty::Bool,
                    UnOp::Neg | UnOp::Plus | UnOp::BitNot => {
                        if !numberish(&t) {
                            let ts = self.show(&t);
                            self.err(span, format!("unary operator requires a number, found '{}'", ts));
                        }
                        Ty::Number
                    }
                }
            }
            ExprKind::Binary(op, l, r) => {
                let lt = self.check_expr(l, None);
                let rt = self.check_expr(r, None);
                self.binary_type(*op, &lt, &rt, span)
            }
            ExprKind::Logical(op, l, r) => {
                let lt = self.check_expr(l, exp);
                match op {
                    LogOp::And => {
                        let n = self.narrow_of(l);
                        let rt = self.with_facts(&n.t, |s| s.check_expr(r, exp));
                        mk_union(vec![falsy_part(&lt), rt])
                    }
                    LogOp::Or => {
                        let n = self.narrow_of(l);
                        let rt = self.with_facts(&n.f, |s| s.check_expr(r, exp));
                        mk_union(vec![truthy_part(&lt), rt])
                    }
                    LogOp::Nullish => {
                        let rt = self.check_expr(r, exp);
                        if !has_null(&lt) && !is_any(&lt) && !matches!(lt, Ty::Unknown) {
                            self.warn(span, "left operand of '??' is never null");
                        }
                        mk_union(vec![remove_null(&lt), rt])
                    }
                }
            }
            ExprKind::Assign { op, target, value } => self.check_assign(*op, target, value, span),
            ExprKind::Update { target, .. } => {
                let tt = self.check_target(target, true);
                if !numberish(&tt) {
                    let ts = self.show(&tt);
                    self.err(span, format!("increment/decrement requires a number, found '{}'", ts));
                }
                if let Some(k) = self.ref_key(target) {
                    self.invalidate(&k);
                }
                Ty::Number
            }
            ExprKind::Cond(c, a, b) => {
                self.check_expr(c, None);
                let n = self.narrow_of(c);
                let at = self.with_facts(&n.t, |s| s.check_expr(a, exp));
                let bt = self.with_facts(&n.f, |s| s.check_expr(b, exp));
                mk_union(vec![at, bt])
            }
            ExprKind::Call { callee, targs, args, optional } => self.check_call(callee, targs, args, *optional, span),
            ExprKind::Member { obj, name, optional } => self.check_member(e, obj, name, *optional, span),
            ExprKind::Index { obj, idx, optional } => self.check_index(obj, idx, *optional, span),
            ExprKind::Chain(inner) => {
                let saved = self.chain_null;
                self.chain_null = false;
                let t = self.check_expr(inner, exp);
                let nullable = self.chain_null;
                self.chain_null = saved;
                if nullable {
                    mk_union(vec![t, Ty::Null])
                } else {
                    t
                }
            }
            ExprKind::Await(x) => {
                let in_async = self.fns.last().map_or(false, |f| f.is_async || f.top_level);
                if !in_async {
                    self.err(span, "'await' is only allowed inside async functions");
                }
                let t = self.check_expr(x, None);
                match t {
                    Ty::Promise(i) => (*i).clone(),
                    Ty::Union(u) if u.iter().any(|m| matches!(m, Ty::Promise(_))) => mk_union(u.iter().map(|m| if let Ty::Promise(i) = m { (**i).clone() } else { m.clone() }).collect()),
                    other => other,
                }
            }
            ExprKind::TypeOf(x) => {
                self.check_expr(x, None);
                Ty::Str
            }
            ExprKind::As(x, te) => {
                let target = self.resolve_type(te);
                let t = self.check_expr(x, Some(&target));
                let ok = matches!(t, Ty::Any | Ty::Unknown | Ty::Error)
                    || self.tcx.assignable(&t, &target)
                    || self.tcx.assignable(&target, &t)
                    || self.tcx.assignable(&widen(&t), &target)
                    || self.tcx.assignable(&target, &widen(&t));
                if !ok {
                    let (a, b) = (self.show(&t), self.show(&target));
                    self.err(span, format!("conversion of type '{}' to type '{}' may be a mistake; the types do not sufficiently overlap", a, b));
                }
                target
            }
            ExprKind::NonNull(x) => {
                let t = self.check_expr(x, None);
                remove_null(&t)
            }
        }
    }

    pub fn check_ident(&mut self, name: &str, span: Span) -> Ty {
        if let Some(l) = self.find_local(name) {
            l.used.set(true);
            let key = format!("@{}", l.id);
            let declared = l.declared.clone();
            return self.facts.get(&key).cloned().unwrap_or(declared);
        }
        let cur = self.cur;
        if let Some(sym) = self.mods[cur].vals.get(name).cloned() {
            self.mark_import_used(name);
            if matches!(*sym.ty.borrow(), TopTy::Namespace(_)) {
                self.err(span, format!("namespace '{}' cannot be used as a value; access one of its members instead", name));
                return Ty::Error;
            }
            // use before declaration at top level
            if sym.module == cur && self.fns.len() == 1 && matches!(*sym.ty.borrow(), TopTy::LazyVar(..)) {
                self.err(span, format!("'{}' is used before its declaration", name));
                return Ty::Error;
            }
            let t = self.sym_type(&sym, span);
            let key = format!("#{}:{}", cur, name);
            return self.facts.get(&key).cloned().unwrap_or(t);
        }
        if let Some(sym) = self.mods[self.prelude_mod].exp_vals.get(name).cloned() {
            return self.sym_type(&sym, span);
        }
        self.err(span, format!("cannot find name '{}'", name));
        Ty::Error
    }

    fn check_array(&mut self, items: &[ArrElem], exp: Option<&Ty>, _span: Span) -> Ty {
        let exp_res: Option<Ty> = exp.map(|t| self.tcx.resolve(t));
        let tuple_exp: Option<Vec<Ty>> = exp_res.as_ref().and_then(|t| {
            members(t).into_iter().find_map(|m| if let Ty::Tuple(v) = m { Some(v.as_ref().clone()) } else { None })
        });
        let elem_exp: Option<Ty> = exp_res.as_ref().and_then(|t| {
            let ms: Vec<Ty> = members(t)
                .into_iter()
                .filter_map(|m| match m {
                    Ty::Array(e) => Some((*e).clone()),
                    _ => None,
                })
                .collect();
            if ms.is_empty() {
                None
            } else {
                Some(mk_union(ms))
            }
        });
        let has_spread = items.iter().any(|i| matches!(i, ArrElem::Spread(_)));
        let mut tys = Vec::new();
        for (i, it) in items.iter().enumerate() {
            match it {
                ArrElem::Item(x) => {
                    let hint = tuple_exp.as_ref().and_then(|v| v.get(i).cloned()).or_else(|| elem_exp.clone());
                    let t = self.check_expr(x, hint.as_ref());
                    let keep = hint.as_ref().map_or(false, has_literal);
                    tys.push(if keep { t } else { widen(&t) });
                }
                ArrElem::Spread(x) => {
                    let t = self.check_expr(x, None);
                    match self.tcx.resolve(&t) {
                        Ty::Array(e) => tys.push((*e).clone()),
                        Ty::Tuple(v) => tys.push(mk_union(v.iter().cloned().collect())),
                        Ty::Str | Ty::LitStr(_) => tys.push(Ty::Str),
                        Ty::Any | Ty::Error => tys.push(Ty::Any),
                        other => {
                            let ts = self.show(&other);
                            self.err(x.span, format!("type '{}' is not iterable", ts));
                        }
                    }
                }
            }
        }
        if let Some(v) = &tuple_exp {
            if !has_spread && v.len() == tys.len() {
                return Ty::Tuple(Rc::new(tys));
            }
        }
        if tys.is_empty() {
            return Ty::Array(Rc::new(Ty::Never));
        }
        Ty::Array(Rc::new(mk_union(tys)))
    }

    fn check_object(&mut self, props: &[ObjProp], exp: Option<&Ty>, span: Span) -> Ty {
        let mut o = ObjTy::default();
        let mut any_spread = false;
        for p in props {
            match p {
                ObjProp::KV(k, v) => {
                    let pe = self.prop_expected(exp, k);
                    let t = self.check_expr(v, pe.as_ref());
                    let keep = pe.as_ref().map_or(false, has_literal);
                    o.props.insert(k.clone(), Prop { ty: if keep { t } else { widen(&t) }, optional: false });
                }
                ObjProp::Spread(x) => {
                    let t = self.check_expr(x, None);
                    match self.tcx.resolve(&t) {
                        Ty::Obj(src) => {
                            for (k, p) in src.props.iter() {
                                o.props.insert(k.clone(), p.clone());
                            }
                            if src.index.is_some() {
                                o.index = src.index.clone();
                            }
                        }
                        Ty::Any | Ty::Error => any_spread = true,
                        other => {
                            let ts = self.show(&other);
                            self.err(x.span, format!("spread expression must be an object, found '{}'", ts));
                        }
                    }
                }
            }
        }
        // excess property check against the contextual type
        if let (Some(t), false) = (exp, any_spread) {
            if !props.iter().any(|p| matches!(p, ObjProp::Spread(_))) {
                let mut targets: Vec<Rc<ObjTy>> = Vec::new();
                let mut opaque = false;
                for m in members(t) {
                    match self.tcx.resolve(&m) {
                        Ty::Obj(ot) => targets.push(ot),
                        Ty::Null => {}
                        _ => opaque = true,
                    }
                }
                if !opaque && !targets.is_empty() && targets.iter().all(|t| t.index.is_none()) {
                    for p in props {
                        if let ObjProp::KV(k, v) = p {
                            if !targets.iter().any(|t| t.props.contains_key(k)) {
                                let ts = self.show(t);
                                self.err(v.span, format!("object literal may only specify known properties; '{}' does not exist in type '{}'", k, ts));
                            }
                        }
                    }
                }
            }
        }
        let _ = span;
        Ty::Obj(Rc::new(o))
    }

    pub fn binary_type(&mut self, op: BinOp, lt: &Ty, rt: &Ty, span: Span) -> Ty {
        let (ls, rs) = (self.show(lt), self.show(rt));
        match op {
            BinOp::Add => {
                if stringy(lt) && !is_any(lt) || stringy(rt) && !is_any(rt) {
                    let other = if stringy(lt) && !is_any(lt) { rt } else { lt };
                    if !stringifiable(other) {
                        self.err(span, format!("operator '+' cannot be applied to types '{}' and '{}'; convert the value to a string first", ls, rs));
                    }
                    Ty::Str
                } else if is_any(lt) || is_any(rt) {
                    Ty::Any
                } else if numberish(lt) && numberish(rt) {
                    Ty::Number
                } else {
                    self.err(span, format!("operator '+' cannot be applied to types '{}' and '{}'", ls, rs));
                    Ty::Error
                }
            }
            BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod | BinOp::Pow | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
                if !numberish(lt) || !numberish(rt) {
                    self.err(span, format!("arithmetic operator requires numbers, found '{}' and '{}'", ls, rs));
                }
                Ty::Number
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let ok = (numberish(lt) && numberish(rt)) || (stringy(lt) && stringy(rt)) || is_any(lt) || is_any(rt);
                if !ok {
                    self.err(span, format!("cannot compare '{}' and '{}'; both operands must be numbers or both strings", ls, rs));
                }
                Ty::Bool
            }
            BinOp::Eq | BinOp::Ne => {
                if !self.comparable(lt, rt) {
                    self.err(span, format!("this comparison is always {} because the types '{}' and '{}' have no overlap", if op == BinOp::Eq { "false" } else { "true" }, ls, rs));
                }
                Ty::Bool
            }
        }
    }

    fn comparable(&self, a: &Ty, b: &Ty) -> bool {
        if is_any(a) || is_any(b) || matches!(a, Ty::Unknown) || matches!(b, Ty::Unknown) {
            return true;
        }
        if has_null(a) || has_null(b) {
            return true;
        }
        for x in members(a) {
            for y in members(b) {
                let (wx, wy) = (widen(&x), widen(&y));
                if matches!(x, Ty::Param(_)) || matches!(y, Ty::Param(_)) {
                    return true;
                }
                if self.tcx.assignable(&wx, &wy) || self.tcx.assignable(&wy, &wx) {
                    // literal vs different literal of same base: no overlap
                    match (&x, &y) {
                        (Ty::LitStr(p), Ty::LitStr(q)) if p != q => continue,
                        (Ty::LitNum(p), Ty::LitNum(q)) if p != q => continue,
                        (Ty::LitBool(p), Ty::LitBool(q)) if p != q => continue,
                        _ => return true,
                    }
                }
            }
        }
        false
    }

    // ---------------- assignment ----------------
    /// Type of an assignment/update target, validating mutability.
    fn check_target(&mut self, target: &Expr, read_too: bool) -> Ty {
        let _ = read_too;
        match &target.kind {
            ExprKind::Ident(n) => {
                if let Some(l) = self.find_local(n) {
                    l.used.set(true);
                    let (declared, mutable) = (l.declared.clone(), l.mutable);
                    if !mutable {
                        self.err(target.span, format!("cannot assign to '{}' because it is a constant", n));
                        return Ty::Error;
                    }
                    return declared;
                }
                let cur = self.cur;
                if let Some(sym) = self.mods[cur].vals.get(n).cloned() {
                    self.mark_import_used(n);
                    if sym.module != cur {
                        self.err(target.span, format!("cannot assign to '{}' because it is an imported binding", n));
                        return Ty::Error;
                    }
                    if !sym.mutable {
                        self.err(target.span, format!("cannot assign to '{}' because it is a constant or function", n));
                        return Ty::Error;
                    }
                    return self.sym_type(&sym, target.span);
                }
                self.err(target.span, format!("cannot find name '{}'", n));
                Ty::Error
            }
            ExprKind::Member { obj, name, .. } => {
                let bt = self.check_expr(obj, None);
                let bt = self.tcx.resolve(&bt);
                match &bt {
                    Ty::Obj(o) => {
                        if let Some(p) = o.props.get(name) {
                            if p.optional {
                                return mk_union(vec![p.ty.clone(), Ty::Null]);
                            }
                            return p.ty.clone();
                        }
                        if let Some(i) = &o.index {
                            return i.clone();
                        }
                        let ts = self.show(&bt);
                        self.err(target.span, format!("property '{}' does not exist on type '{}'", name, ts));
                        Ty::Error
                    }
                    Ty::Any | Ty::Error => Ty::Any,
                    other => {
                        let ts = self.show(other);
                        self.err(target.span, format!("cannot assign to property '{}' of type '{}'", name, ts));
                        Ty::Error
                    }
                }
            }
            ExprKind::Index { obj, idx, .. } => {
                let bt = self.check_expr(obj, None);
                let it = self.check_expr(idx, None);
                let bt = self.tcx.resolve(&bt);
                match &bt {
                    Ty::Array(e) => {
                        if !numberish(&it) {
                            self.err(idx.span, "array index must be a number");
                        }
                        (**e).clone()
                    }
                    Ty::Obj(o) => {
                        if !stringy(&it) {
                            self.err(idx.span, "object key must be a string");
                        }
                        if let Ty::LitStr(k) = &it {
                            if let Some(p) = o.props.get(&**k) {
                                return if p.optional { mk_union(vec![p.ty.clone(), Ty::Null]) } else { p.ty.clone() };
                            }
                        }
                        match &o.index {
                            Some(i) => i.clone(),
                            None => {
                                let ts = self.show(&bt);
                                self.err(target.span, format!("type '{}' has no index signature; cannot assign using a computed key", ts));
                                Ty::Error
                            }
                        }
                    }
                    Ty::Any | Ty::Error => Ty::Any,
                    other => {
                        let ts = self.show(other);
                        self.err(target.span, format!("cannot assign through an index on type '{}'", ts));
                        Ty::Error
                    }
                }
            }
            _ => {
                self.err(target.span, "invalid assignment target");
                Ty::Error
            }
        }
    }

    fn narrow_assigned(&self, declared: &Ty, assigned: &Ty) -> Ty {
        let dm = members(declared);
        if dm.len() < 2 {
            return declared.clone();
        }
        let mut out = Vec::new();
        for am in members(assigned) {
            for d in &dm {
                if self.tcx.assignable(&am, d) {
                    out.push(d.clone());
                } else if self.tcx.assignable(&widen(&am), d) {
                    out.push(d.clone());
                }
            }
        }
        if out.is_empty() {
            declared.clone()
        } else {
            mk_union(out)
        }
    }

    fn check_assign(&mut self, op: AssignOp, target: &Expr, value: &Expr, span: Span) -> Ty {
        let tt = self.check_target(target, !matches!(op, AssignOp::Assign));
        let key = self.ref_key(target);
        let result = match op {
            AssignOp::Assign => {
                let vt = self.check_expr(value, Some(&tt));
                self.expect(&vt, &tt, value.span, "assignment");
                if let Some(k) = &key {
                    self.invalidate(k);
                    let n = self.narrow_assigned(&tt, &vt);
                    if n != tt {
                        self.facts.insert(k.clone(), n);
                    }
                }
                return vt;
            }
            AssignOp::Bin(b) => {
                let vt = self.check_expr(value, None);
                let rt = self.binary_type(b, &tt, &vt, span);
                self.expect(&rt, &tt, span, "compound assignment");
                rt
            }
            AssignOp::Log(l) => {
                let vt = self.check_expr(value, Some(&tt));
                match l {
                    LogOp::Nullish => {
                        let target_ty = remove_null(&tt);
                        self.expect(&vt, &target_ty, value.span, "assignment");
                    }
                    _ => {
                        self.expect(&vt, &tt, value.span, "assignment");
                    }
                }
                tt.clone()
            }
        };
        if let Some(k) = key {
            self.invalidate(&k);
        }
        result
    }

    // ---------------- member / index ----------------
    fn check_member(&mut self, whole: &Expr, obj: &Expr, name: &str, optional: bool, span: Span) -> Ty {
        // namespace access
        if let ExprKind::Ident(n) = &obj.kind {
            if self.find_local(n).is_none() {
                let cur = self.cur;
                if let Some(sym) = self.mods[cur].vals.get(n).cloned() {
                    let ns = if let TopTy::Namespace(m) = &*sym.ty.borrow() { Some(*m) } else { None };
                    if let Some(m) = ns {
                        self.mark_import_used(n);
                        return match self.mods[m].exp_vals.get(name).cloned() {
                            Some(s) => self.sym_type(&s, span),
                            None => {
                                self.err(span, format!("module '{}' has no exported member '{}'", n, name));
                                Ty::Error
                            }
                        };
                    }
                }
            }
        }
        let mut bt = self.check_expr(obj, None);
        if optional {
            if has_null(&bt) {
                self.chain_null = true;
                bt = remove_null(&bt);
            }
        }
        let key = self.ref_key(whole);
        let result = match self.get_member(&bt, name) {
            Ok(t) => t,
            Err(msg) => {
                self.err(span, msg);
                return Ty::Error;
            }
        };
        if let Some(k) = key {
            if let Some(t) = self.facts.get(&k) {
                return t.clone();
            }
        }
        result
    }

    fn check_index(&mut self, obj: &Expr, idx: &Expr, optional: bool, span: Span) -> Ty {
        let mut bt = self.check_expr(obj, None);
        let it = self.check_expr(idx, None);
        if optional && has_null(&bt) {
            self.chain_null = true;
            bt = remove_null(&bt);
        }
        self.index_type(&bt, &it, span)
    }

    fn index_type(&mut self, bt: &Ty, it: &Ty, span: Span) -> Ty {
        let bt = self.tcx.resolve(bt);
        match &bt {
            Ty::Any | Ty::Error => Ty::Any,
            Ty::Array(e) => {
                if !numberish(it) {
                    let ts = self.show(it);
                    self.err(span, format!("array index must be a number, found '{}'", ts));
                }
                (**e).clone()
            }
            Ty::Tuple(v) => {
                if let Ty::LitNum(n) = it {
                    let i = *n as usize;
                    if n.fract() == 0.0 && *n >= 0.0 && i < v.len() {
                        return v[i].clone();
                    }
                    self.err(span, format!("tuple index {} is out of range (length {})", crate::value::fmt_num(*n), v.len()));
                    return Ty::Error;
                }
                if !numberish(it) {
                    self.err(span, "tuple index must be a number");
                }
                mk_union(v.iter().cloned().collect())
            }
            Ty::Str | Ty::LitStr(_) => {
                if !numberish(it) {
                    self.err(span, "string index must be a number");
                }
                Ty::Str
            }
            Ty::Obj(o) => {
                if !stringy(it) {
                    let ts = self.show(it);
                    self.err(span, format!("object key must be a string, found '{}'", ts));
                    return Ty::Error;
                }
                if let Ty::LitStr(k) = it {
                    if let Some(p) = o.props.get(&**k) {
                        return if p.optional { mk_union(vec![p.ty.clone(), Ty::Null]) } else { p.ty.clone() };
                    }
                }
                match &o.index {
                    Some(i) => i.clone(),
                    None => {
                        let ts = self.show(&bt);
                        self.err(span, format!("type '{}' has no index signature; use a literal key or declare Record<string, T>", ts));
                        Ty::Error
                    }
                }
            }
            Ty::Union(u) => {
                if u.iter().any(|m| matches!(m, Ty::Null)) {
                    self.err(span, "object is possibly 'null' (use '?.' or check for null)");
                    return Ty::Error;
                }
                let items: Vec<Ty> = u.iter().map(|m| self.index_type(m, it, span)).collect();
                mk_union(items)
            }
            Ty::Null => {
                self.err(span, "cannot index into null");
                Ty::Error
            }
            other => {
                let ts = self.show(other);
                self.err(span, format!("type '{}' cannot be indexed", ts));
                Ty::Error
            }
        }
    }

    // ---------------- calls ----------------
    fn is_ctx_lambda(e: &Expr) -> bool {
        matches!(&e.kind, ExprKind::Func(f) if f.params.iter().any(|p| p.ty.is_none()))
    }

    fn partial_subst(&self, t: &Ty, b: &HashMap<Rc<str>, Ty>, tps: &[Rc<str>]) -> Ty {
        let mut m = b.clone();
        for tp in tps {
            m.entry(tp.clone()).or_insert(Ty::Unknown);
        }
        subst(t, &m)
    }

    fn check_call(&mut self, callee: &Expr, targs: &[TypeExpr], args: &[Arg], optional: bool, span: Span) -> Ty {
        let mut ct = self.check_expr(callee, None);
        if optional && has_null(&ct) {
            self.chain_null = true;
            ct = remove_null(&ct);
        }
        match ct {
            Ty::Any | Ty::Error => {
                for a in args {
                    match a {
                        Arg::Item(e) | Arg::Spread(e) => {
                            self.check_expr(e, None);
                        }
                    }
                }
                Ty::Any
            }
            Ty::Func(f) => self.apply_fn(&f, targs, args, span),
            other => {
                let ts = self.show(&other);
                self.err(span, format!("this expression is not callable (type '{}' has no call signature)", ts));
                for a in args {
                    match a {
                        Arg::Item(e) | Arg::Spread(e) => {
                            self.check_expr(e, None);
                        }
                    }
                }
                Ty::Error
            }
        }
    }

    pub fn apply_fn(&mut self, f: &Rc<FnTy>, targs: &[TypeExpr], args: &[Arg], span: Span) -> Ty {
        let tps: Vec<Rc<str>> = f.tparams.iter().map(|t| t.name.clone()).collect();
        let mut b: HashMap<Rc<str>, Ty> = HashMap::new();
        if !targs.is_empty() {
            if targs.len() != tps.len() {
                self.err(span, format!("expected {} type argument(s), found {}", tps.len(), targs.len()));
            } else {
                for (tp, te) in tps.iter().zip(targs.iter()) {
                    let t = self.resolve_type(te);
                    b.insert(tp.clone(), t);
                }
            }
        }
        let has_spread = args.iter().any(|a| matches!(a, Arg::Spread(_)));
        let required = f.params.iter().filter(|p| !p.optional && !p.rest).count();
        let has_rest = f.params.last().map_or(false, |p| p.rest);
        let fixed = f.params.iter().filter(|p| !p.rest).count();
        if !has_spread {
            if args.len() < required {
                self.err(span, format!("expected {}{} argument(s), but got {}", if has_rest || fixed > required { "at least " } else { "" }, required, args.len()));
            } else if !has_rest && args.len() > fixed {
                self.err(span, format!("expected {} argument(s), but got {}", if fixed > required { format!("{} to {}", required, fixed) } else { fixed.to_string() }, args.len()));
            }
        }
        let formal_for = |i: usize| -> Option<Ty> {
            if i < fixed {
                Some(f.params[i].ty.clone())
            } else if has_rest {
                Some(f.params.last().unwrap().ty.clone())
            } else {
                None
            }
        };
        let mut arg_tys: Vec<Option<Ty>> = vec![None; args.len()];
        for pass in 0..2 {
            for (i, a) in args.iter().enumerate() {
                match a {
                    Arg::Spread(e) => {
                        if pass == 0 {
                            let t = self.check_expr(e, None);
                            match self.tcx.resolve(&t) {
                                Ty::Array(_) | Ty::Tuple(_) | Ty::Any | Ty::Error => {}
                                other => {
                                    let ts = self.show(&other);
                                    self.err(e.span, format!("spread argument must be an array, found '{}'", ts));
                                }
                            }
                            arg_tys[i] = Some(t);
                        }
                    }
                    Arg::Item(e) => {
                        let lambda = Self::is_ctx_lambda(e);
                        if (pass == 0) == lambda {
                            continue;
                        }
                        let formal = formal_for(i);
                        let hint = formal.as_ref().map(|fm| self.partial_subst(fm, &b, &tps));
                        let t = self.check_expr(e, hint.as_ref());
                        if let Some(fm) = &formal {
                            self.infer(fm, &t, &tps, &mut b);
                        }
                        arg_tys[i] = Some(t);
                    }
                }
            }
        }
        // finalize bindings
        for tp in &f.tparams {
            if !b.contains_key(&tp.name) {
                let fallback = match &tp.constraint {
                    Some(c) => subst(c, &b),
                    None => Ty::Unknown,
                };
                b.insert(tp.name.clone(), fallback);
            }
        }
        for tp in &f.tparams {
            if let Some(c) = &tp.constraint {
                let bound = b.get(&tp.name).cloned().unwrap_or(Ty::Unknown);
                let cs = subst(c, &b);
                if !self.tcx.assignable(&bound, &cs) {
                    let (x, y) = (self.show(&bound), self.show(&cs));
                    self.err(span, format!("type '{}' does not satisfy the constraint '{}'", x, y));
                }
            }
        }
        for (i, a) in args.iter().enumerate() {
            if let (Arg::Item(e), Some(t)) = (a, &arg_tys[i]) {
                if let Some(fm) = formal_for(i) {
                    let fin = subst(&fm, &b);
                    if !self.tcx.assignable(t, &fin) {
                        let (x, y) = (self.show(t), self.show(&fin));
                        self.err(e.span, format!("argument of type '{}' is not assignable to parameter of type '{}'", x, y));
                    }
                }
            }
        }
        subst(&f.ret, &b)
    }

    fn bind_infer(&self, p: &Rc<str>, actual: &Ty, b: &mut HashMap<Rc<str>, Ty>) {
        if matches!(actual, Ty::Never | Ty::Error) {
            return;
        }
        let keep_literal = self.tcx.constraints.get(p).map_or(false, has_literal);
        let a = if keep_literal { actual.clone() } else { widen(actual) };
        match b.get(p) {
            None => {
                b.insert(p.clone(), a);
            }
            Some(old) => {
                if self.tcx.assignable(&a, old) {
                } else if self.tcx.assignable(old, &a) {
                    b.insert(p.clone(), a);
                } else {
                    let u = mk_union(vec![old.clone(), a]);
                    b.insert(p.clone(), u);
                }
            }
        }
    }

    pub fn infer(&self, formal: &Ty, actual: &Ty, tps: &[Rc<str>], b: &mut HashMap<Rc<str>, Ty>) {
        match formal {
            Ty::Param(p) if tps.contains(p) => self.bind_infer(p, actual, b),
            Ty::Array(f) => match self.tcx.resolve(actual) {
                Ty::Array(a) => self.infer(f, &a, tps, b),
                Ty::Tuple(v) => self.infer(f, &mk_union(v.iter().cloned().collect()), tps, b),
                _ => {}
            },
            Ty::Tuple(fv) => {
                if let Ty::Tuple(av) = actual {
                    for (x, y) in fv.iter().zip(av.iter()) {
                        self.infer(x, y, tps, b);
                    }
                }
            }
            Ty::Promise(f) => {
                if let Ty::Promise(a) = actual {
                    self.infer(f, a, tps, b);
                }
            }
            Ty::Union(fu) => {
                let params: Vec<&Ty> = fu.iter().filter(|m| matches!(m, Ty::Param(p) if tps.contains(p))).collect();
                let others: Vec<&Ty> = fu.iter().filter(|m| !matches!(m, Ty::Param(p) if tps.contains(p))).collect();
                for am in members(actual) {
                    if others.iter().any(|o| !contains_param(o, tps) && self.tcx.assignable(&am, o)) {
                        continue;
                    }
                    let mut done = false;
                    for o in others.iter().filter(|o| contains_param(o, tps)) {
                        if same_shape(o, &am) {
                            self.infer(o, &am, tps, b);
                            done = true;
                            break;
                        }
                    }
                    if !done {
                        if let Some(pm) = params.first() {
                            self.infer(pm, &am, tps, b);
                        }
                    }
                }
            }
            Ty::Obj(fo) => {
                if let Ty::Obj(ao) = self.tcx.resolve(actual) {
                    for (k, fp) in fo.props.iter() {
                        if let Some(ap) = ao.props.get(k) {
                            self.infer(&fp.ty, &ap.ty, tps, b);
                        }
                    }
                    if let Some(fi) = &fo.index {
                        let mut vals: Vec<Ty> = ao.props.values().map(|p| p.ty.clone()).collect();
                        if let Some(ai) = &ao.index {
                            vals.push(ai.clone());
                        }
                        if !vals.is_empty() {
                            let u = mk_union(vals.iter().map(widen).collect());
                            self.infer(fi, &u, tps, b);
                        }
                    }
                }
            }
            Ty::Func(ff) => {
                if let Ty::Func(af) = actual {
                    for (i, fp) in ff.params.iter().enumerate() {
                        if let Some(ap) = af.params.get(i) {
                            self.infer(&fp.ty, &ap.ty, tps, b);
                        }
                    }
                    self.infer(&ff.ret, &af.ret, tps, b);
                }
            }
            Ty::Ref(id, fargs) => match actual {
                Ty::Ref(id2, aargs) if id == id2 => {
                    for (x, y) in fargs.iter().zip(aargs.iter()) {
                        self.infer(x, y, tps, b);
                    }
                }
                _ => {
                    let fo = Ty::Obj(self.tcx.expand(*id, fargs));
                    self.infer(&fo, actual, tps, b);
                }
            },
            _ => {}
        }
    }

    // ---------------- narrowing ----------------
    pub fn narrow_of(&mut self, e: &Expr) -> Narrow {
        match &e.kind {
            ExprKind::Unary(UnOp::Not, x) => self.narrow_of(x).swap(),
            ExprKind::Logical(LogOp::And, a, b) => {
                let na = self.narrow_of(a);
                let nb = {
                    let t = na.t.clone();
                    self.with_facts(&t, |s| s.narrow_of(b))
                };
                let mut t = na.t;
                t.extend(nb.t);
                Narrow { t, f: vec![] }
            }
            ExprKind::Logical(LogOp::Or, a, b) => {
                let na = self.narrow_of(a);
                let nb = {
                    let f = na.f.clone();
                    self.with_facts(&f, |s| s.narrow_of(b))
                };
                let mut f = na.f;
                f.extend(nb.f);
                Narrow { t: vec![], f }
            }
            ExprKind::Binary(op @ (BinOp::Eq | BinOp::Ne), l, r) => {
                let n = self.narrow_eq(l, r).or_else(|| self.narrow_eq(r, l)).unwrap_or_else(Narrow::none);
                if *op == BinOp::Ne {
                    n.swap()
                } else {
                    n
                }
            }
            ExprKind::Ident(_) | ExprKind::Member { optional: false, .. } => {
                let Some(key) = self.ref_key(e) else { return Narrow::none() };
                let ty = self.quiet_type(e);
                Narrow { t: vec![(key.clone(), truthy_part(&ty))], f: vec![(key, falsy_part(&ty))] }
            }
            _ => Narrow::none(),
        }
    }

    /// Narrowing for `lhs === rhs` where `rhs` is a literal-ish expression.
    fn narrow_eq(&mut self, lhs: &Expr, rhs: &Expr) -> Option<Narrow> {
        match &rhs.kind {
            ExprKind::Null => {
                let key = self.ref_key(lhs)?;
                let ty = self.quiet_type(lhs);
                let only_null = if matches!(ty, Ty::Unknown | Ty::Any) { Ty::Null } else { mk_union(members(&ty).into_iter().filter(|m| matches!(m, Ty::Null)).collect()) };
                Some(Narrow { t: vec![(key.clone(), only_null)], f: vec![(key, remove_null(&ty))] })
            }
            ExprKind::Str(_) | ExprKind::Num(_) | ExprKind::Bool(_) => {
                let lit = match &rhs.kind {
                    ExprKind::Str(s) => Ty::LitStr(Rc::from(s.as_str())),
                    ExprKind::Num(n) => Ty::LitNum(*n),
                    ExprKind::Bool(b) => Ty::LitBool(*b),
                    _ => unreachable!(),
                };
                // typeof x === "string"
                if let (ExprKind::TypeOf(inner), ExprKind::Str(s)) = (&lhs.kind, &rhs.kind) {
                    return self.narrow_typeof(inner, s);
                }
                self.narrow_lit(lhs, &lit)
            }
            _ => None,
        }
    }

    fn narrow_lit(&mut self, x: &Expr, lit: &Ty) -> Option<Narrow> {
        let mut t = Vec::new();
        let mut f = Vec::new();
        // discriminant property: x.kind === "a"
        if let ExprKind::Member { obj, name, optional: false } = &x.kind {
            if let Some(base_key) = self.ref_key(obj) {
                let bt = self.quiet_type(obj);
                let ms = members(&bt);
                if ms.len() > 1 {
                    let mut tm = Vec::new();
                    let mut fm = Vec::new();
                    for m in &ms {
                        let pt = match self.tcx.resolve(m) {
                            Ty::Obj(o) => o.props.get(name).map(|p| p.ty.clone()),
                            _ => None,
                        };
                        match pt {
                            Some(pt) => {
                                if self.tcx.assignable(lit, &pt) {
                                    tm.push(m.clone());
                                }
                                if pt != *lit {
                                    fm.push(m.clone());
                                }
                            }
                            None => {
                                tm.push(m.clone());
                                fm.push(m.clone());
                            }
                        }
                    }
                    t.push((base_key.clone(), mk_union(tm)));
                    f.push((base_key, mk_union(fm)));
                }
            }
        }
        if let Some(key) = self.ref_key(x) {
            let ty = self.quiet_type(x);
            let ms = members(&ty);
            let tm: Vec<Ty> = ms
                .iter()
                .filter_map(|m| match m {
                    Ty::Unknown | Ty::Any => Some(lit.clone()),
                    m if self.tcx.assignable(lit, m) => Some(lit.clone()),
                    _ => None,
                })
                .collect();
            let fm: Vec<Ty> = ms.iter().filter(|m| *m != lit).cloned().collect();
            t.push((key.clone(), mk_union(tm)));
            f.push((key, mk_union(fm)));
        }
        if t.is_empty() && f.is_empty() {
            None
        } else {
            Some(Narrow { t, f })
        }
    }

    fn narrow_typeof(&mut self, x: &Expr, s: &str) -> Option<Narrow> {
        let key = self.ref_key(x)?;
        let ty = self.quiet_type(x);
        let matches_kind = |m: &Ty| -> Option<bool> {
            Some(match (s, m) {
                ("string", Ty::Str | Ty::LitStr(_)) => true,
                ("number", Ty::Number | Ty::LitNum(_)) => true,
                ("boolean", Ty::Bool | Ty::LitBool(_)) => true,
                ("null", Ty::Null) => true,
                ("array", Ty::Array(_) | Ty::Tuple(_)) => true,
                ("object", Ty::Obj(_) | Ty::Ref(..) | Ty::Promise(_)) => true,
                ("function", Ty::Func(_)) => true,
                (_, Ty::Any | Ty::Unknown | Ty::Param(_) | Ty::Error) => return None,
                _ => false,
            })
        };
        let unknown_to = match s {
            "string" => Some(Ty::Str),
            "number" => Some(Ty::Number),
            "boolean" => Some(Ty::Bool),
            "null" => Some(Ty::Null),
            "array" => Some(Ty::Array(Rc::new(Ty::Unknown))),
            "object" => Some(Ty::Obj(Rc::new(ObjTy { props: Default::default(), index: Some(Ty::Unknown) }))),
            _ => None,
        };
        let mut tm = Vec::new();
        let mut fm = Vec::new();
        for m in members(&ty) {
            match matches_kind(&m) {
                Some(true) => tm.push(m),
                Some(false) => fm.push(m),
                None => {
                    if matches!(m, Ty::Unknown | Ty::Any) {
                        if let Some(u) = &unknown_to {
                            tm.push(u.clone());
                        }
                    } else {
                        tm.push(m.clone());
                    }
                    fm.push(m);
                }
            }
        }
        Some(Narrow { t: vec![(key.clone(), mk_union(tm))], f: vec![(key, mk_union(fm))] })
    }
}

fn same_shape(a: &Ty, b: &Ty) -> bool {
    matches!(
        (a, b),
        (Ty::Array(_), Ty::Array(_)) | (Ty::Promise(_), Ty::Promise(_)) | (Ty::Func(_), Ty::Func(_)) | (Ty::Obj(_), Ty::Obj(_)) | (Ty::Obj(_), Ty::Ref(..)) | (Ty::Ref(..), Ty::Obj(_)) | (Ty::Ref(..), Ty::Ref(..)) | (Ty::Tuple(_), Ty::Tuple(_))
    )
}
