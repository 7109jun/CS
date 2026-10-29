use indexmap::IndexMap;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Any,
    Unknown,
    Never,
    Void,
    Number,
    Str,
    Bool,
    Null,
    LitStr(Rc<str>),
    LitNum(f64),
    LitBool(bool),
    Array(Rc<Ty>),
    Tuple(Rc<Vec<Ty>>),
    Obj(Rc<ObjTy>),
    Union(Rc<Vec<Ty>>),
    Func(Rc<FnTy>),
    Promise(Rc<Ty>),
    /// A generic type parameter, identified by a unique name (`T#12`).
    Param(Rc<str>),
    /// A named interface instantiation.
    Ref(usize, Rc<Vec<Ty>>),
    /// Error recovery type: compatible with everything.
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Prop {
    pub ty: Ty,
    pub optional: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ObjTy {
    pub props: IndexMap<String, Prop>,
    pub index: Option<Ty>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParamTy {
    pub name: Rc<str>,
    pub ty: Ty,
    pub optional: bool,
    pub rest: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TParam {
    pub name: Rc<str>,
    pub constraint: Option<Ty>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FnTy {
    pub tparams: Vec<TParam>,
    pub params: Vec<ParamTy>,
    pub ret: Ty,
}

pub struct IfaceInfo {
    pub name: String,
    pub tparams: Vec<Rc<str>>,
    pub body: Option<Rc<ObjTy>>,
}

#[derive(Default)]
pub struct TypeCtx {
    pub ifaces: Vec<IfaceInfo>,
    pub constraints: HashMap<Rc<str>, Ty>,
}

pub fn mk_union(items: Vec<Ty>) -> Ty {
    let mut flat: Vec<Ty> = Vec::new();
    fn push(flat: &mut Vec<Ty>, t: Ty) {
        match t {
            Ty::Union(u) => {
                for x in u.iter() {
                    push(flat, x.clone());
                }
            }
            Ty::Never => {}
            other => {
                if !flat.contains(&other) {
                    flat.push(other);
                }
            }
        }
    }
    for t in items {
        push(&mut flat, t);
    }
    if flat.iter().any(|t| matches!(t, Ty::Error)) {
        return Ty::Error;
    }
    if flat.iter().any(|t| matches!(t, Ty::Any)) {
        return Ty::Any;
    }
    if flat.iter().any(|t| matches!(t, Ty::Unknown)) {
        return Ty::Unknown;
    }
    let has_str = flat.contains(&Ty::Str);
    let has_num = flat.contains(&Ty::Number);
    let has_bool = flat.contains(&Ty::Bool);
    flat.retain(|t| match t {
        Ty::LitStr(_) => !has_str,
        Ty::LitNum(_) => !has_num,
        Ty::LitBool(_) => !has_bool,
        _ => true,
    });
    if !has_bool && flat.contains(&Ty::LitBool(true)) && flat.contains(&Ty::LitBool(false)) {
        flat.retain(|t| !matches!(t, Ty::LitBool(_)));
        flat.push(Ty::Bool);
    }
    match flat.len() {
        0 => Ty::Never,
        1 => flat.pop().unwrap(),
        _ => Ty::Union(Rc::new(flat)),
    }
}

pub fn members(t: &Ty) -> Vec<Ty> {
    match t {
        Ty::Union(u) => u.as_ref().clone(),
        Ty::Never => vec![],
        other => vec![other.clone()],
    }
}

pub fn widen(t: &Ty) -> Ty {
    match t {
        Ty::LitStr(_) => Ty::Str,
        Ty::LitNum(_) => Ty::Number,
        Ty::LitBool(_) => Ty::Bool,
        Ty::Array(e) => Ty::Array(Rc::new(widen(e))),
        Ty::Tuple(v) => Ty::Tuple(Rc::new(v.iter().map(widen).collect())),
        Ty::Union(u) => mk_union(u.iter().map(widen).collect()),
        other => other.clone(),
    }
}

pub fn has_null(t: &Ty) -> bool {
    members(t).iter().any(|m| matches!(m, Ty::Null))
}

pub fn remove_null(t: &Ty) -> Ty {
    mk_union(members(t).into_iter().filter(|m| !matches!(m, Ty::Null)).collect())
}

pub fn subst(t: &Ty, map: &HashMap<Rc<str>, Ty>) -> Ty {
    if map.is_empty() {
        return t.clone();
    }
    match t {
        Ty::Param(n) => map.get(n).cloned().unwrap_or_else(|| t.clone()),
        Ty::Array(e) => Ty::Array(Rc::new(subst(e, map))),
        Ty::Tuple(v) => Ty::Tuple(Rc::new(v.iter().map(|x| subst(x, map)).collect())),
        Ty::Promise(e) => mk_promise(subst(e, map)),
        Ty::Union(u) => mk_union(u.iter().map(|x| subst(x, map)).collect()),
        Ty::Obj(o) => Ty::Obj(Rc::new(ObjTy {
            props: o.props.iter().map(|(k, p)| (k.clone(), Prop { ty: subst(&p.ty, map), optional: p.optional })).collect(),
            index: o.index.as_ref().map(|i| subst(i, map)),
        })),
        Ty::Func(f) => Ty::Func(Rc::new(FnTy {
            tparams: f
                .tparams
                .iter()
                .map(|tp| TParam { name: tp.name.clone(), constraint: tp.constraint.as_ref().map(|c| subst(c, map)) })
                .collect(),
            params: f.params.iter().map(|p| ParamTy { name: p.name.clone(), ty: subst(&p.ty, map), optional: p.optional, rest: p.rest }).collect(),
            ret: subst(&f.ret, map),
        })),
        Ty::Ref(id, args) => Ty::Ref(*id, Rc::new(args.iter().map(|a| subst(a, map)).collect())),
        other => other.clone(),
    }
}

pub fn mk_promise(inner: Ty) -> Ty {
    match inner {
        Ty::Promise(_) => inner,
        other => Ty::Promise(Rc::new(other)),
    }
}

pub fn contains_param(t: &Ty, names: &[Rc<str>]) -> bool {
    match t {
        Ty::Param(n) => names.contains(n),
        Ty::Array(e) | Ty::Promise(e) => contains_param(e, names),
        Ty::Union(u) => u.iter().any(|x| contains_param(x, names)),
        Ty::Tuple(v) => v.iter().any(|x| contains_param(x, names)),
        Ty::Obj(o) => o.props.values().any(|p| contains_param(&p.ty, names)) || o.index.as_ref().map_or(false, |i| contains_param(i, names)),
        Ty::Func(f) => f.params.iter().any(|p| contains_param(&p.ty, names)) || contains_param(&f.ret, names),
        Ty::Ref(_, a) => a.iter().any(|x| contains_param(x, names)),
        _ => false,
    }
}

pub fn base_name(n: &str) -> &str {
    n.split('#').next().unwrap_or(n)
}

impl TypeCtx {
    pub fn expand(&self, id: usize, args: &[Ty]) -> Rc<ObjTy> {
        let info = &self.ifaces[id];
        let body = match &info.body {
            Some(b) => b.clone(),
            None => return Rc::new(ObjTy::default()),
        };
        if info.tparams.is_empty() {
            return body;
        }
        let map: HashMap<Rc<str>, Ty> = info.tparams.iter().cloned().zip(args.iter().cloned()).collect();
        match subst(&Ty::Obj(body), &map) {
            Ty::Obj(o) => o,
            _ => Rc::new(ObjTy::default()),
        }
    }

    /// Resolve a Ref to its structural object type; other types are returned unchanged.
    pub fn resolve(&self, t: &Ty) -> Ty {
        match t {
            Ty::Ref(id, args) => Ty::Obj(self.expand(*id, args)),
            other => other.clone(),
        }
    }

    pub fn show(&self, t: &Ty) -> String {
        self.show_d(t, 0)
    }

    fn show_d(&self, t: &Ty, depth: u32) -> String {
        if depth > 4 {
            return "...".into();
        }
        match t {
            Ty::Any => "any".into(),
            Ty::Unknown => "unknown".into(),
            Ty::Never => "never".into(),
            Ty::Void => "void".into(),
            Ty::Number => "number".into(),
            Ty::Str => "string".into(),
            Ty::Bool => "boolean".into(),
            Ty::Null => "null".into(),
            Ty::Error => "<error>".into(),
            Ty::LitStr(s) => format!("\"{}\"", s),
            Ty::LitNum(n) => crate::value::fmt_num(*n),
            Ty::LitBool(b) => b.to_string(),
            Ty::Array(e) => {
                let inner = self.show_d(e, depth + 1);
                if matches!(**e, Ty::Union(_) | Ty::Func(_)) {
                    format!("({})[]", inner)
                } else {
                    format!("{}[]", inner)
                }
            }
            Ty::Promise(e) => format!("Promise<{}>", self.show_d(e, depth + 1)),
            Ty::Tuple(v) => format!("[{}]", v.iter().map(|x| self.show_d(x, depth + 1)).collect::<Vec<_>>().join(", ")),
            Ty::Param(n) => base_name(n).to_string(),
            Ty::Union(u) => u.iter().map(|x| self.show_d(x, depth + 1)).collect::<Vec<_>>().join(" | "),
            Ty::Ref(id, args) => {
                let name = &self.ifaces[*id].name;
                if args.is_empty() {
                    name.clone()
                } else {
                    format!("{}<{}>", name, args.iter().map(|a| self.show_d(a, depth + 1)).collect::<Vec<_>>().join(", "))
                }
            }
            Ty::Obj(o) => {
                if o.props.is_empty() && o.index.is_none() {
                    return "{}".into();
                }
                let mut parts: Vec<String> = o
                    .props
                    .iter()
                    .map(|(k, p)| format!("{}{}: {}", k, if p.optional { "?" } else { "" }, self.show_d(&p.ty, depth + 1)))
                    .collect();
                if let Some(i) = &o.index {
                    parts.push(format!("[key: string]: {}", self.show_d(i, depth + 1)));
                }
                format!("{{ {} }}", parts.join("; "))
            }
            Ty::Func(f) => {
                let tp = if f.tparams.is_empty() {
                    String::new()
                } else {
                    format!("<{}>", f.tparams.iter().map(|p| base_name(&p.name).to_string()).collect::<Vec<_>>().join(", "))
                };
                let ps = f
                    .params
                    .iter()
                    .map(|p| format!("{}{}{}: {}", if p.rest { "..." } else { "" }, p.name, if p.optional { "?" } else { "" }, self.show_d(&p.ty, depth + 1)))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{}({}) => {}", tp, ps, self.show_d(&f.ret, depth + 1))
            }
        }
    }

    pub fn constraint_of(&self, n: &Rc<str>) -> Ty {
        self.constraints.get(n).cloned().unwrap_or(Ty::Unknown)
    }

    pub fn assignable(&self, src: &Ty, dst: &Ty) -> bool {
        self.assignable_d(src, dst, 0)
    }

    fn assignable_d(&self, src: &Ty, dst: &Ty, depth: u32) -> bool {
        if depth > 24 {
            return true;
        }
        if src == dst {
            return true;
        }
        match (src, dst) {
            (Ty::Error, _) | (_, Ty::Error) => return true,
            (_, Ty::Any) | (_, Ty::Unknown) | (Ty::Any, _) | (Ty::Never, _) => return true,
            (Ty::Union(u), _) => return u.iter().all(|m| self.assignable_d(m, dst, depth + 1)),
            _ => {}
        }
        if let Ty::Union(u) = dst {
            if u.iter().any(|m| self.assignable_d(src, m, depth + 1)) {
                return true;
            }
            // an object may satisfy a discriminated union only through one member; handled above.
            return false;
        }
        match (src, dst) {
            (Ty::LitStr(_), Ty::Str) | (Ty::LitNum(_), Ty::Number) | (Ty::LitBool(_), Ty::Bool) => true,
            (Ty::Void, Ty::Void) => true,
            (Ty::Null, Ty::Void) => true,
            (Ty::Array(a), Ty::Array(b)) => self.assignable_d(a, b, depth + 1),
            (Ty::Tuple(a), Ty::Tuple(b)) => a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| self.assignable_d(x, y, depth + 1)),
            (Ty::Tuple(a), Ty::Array(b)) => a.iter().all(|x| self.assignable_d(x, b, depth + 1)),
            (Ty::Promise(a), Ty::Promise(b)) => self.assignable_d(a, b, depth + 1),
            (Ty::Param(a), Ty::Param(b)) if a == b => true,
            (Ty::Param(p), _) => {
                let c = self.constraint_of(p);
                !matches!(c, Ty::Unknown) && self.assignable_d(&c, dst, depth + 1)
            }
            (Ty::Ref(a, aa), Ty::Ref(b, ba)) if a == b => aa.iter().zip(ba.iter()).all(|(x, y)| self.assignable_d(x, y, depth + 1)),
            (Ty::Ref(..), _) | (_, Ty::Ref(..)) => {
                let s = self.resolve(src);
                let d = self.resolve(dst);
                if matches!(s, Ty::Ref(..)) || matches!(d, Ty::Ref(..)) {
                    return false;
                }
                self.assignable_d(&s, &d, depth + 1)
            }
            (Ty::Obj(s), Ty::Obj(d)) => {
                for (k, dp) in d.props.iter() {
                    match s.props.get(k) {
                        Some(sp) => {
                            if sp.optional && !dp.optional {
                                return false;
                            }
                            let st = if sp.optional { mk_union(vec![sp.ty.clone(), Ty::Null]) } else { sp.ty.clone() };
                            let dt = if dp.optional { mk_union(vec![dp.ty.clone(), Ty::Null]) } else { dp.ty.clone() };
                            if !self.assignable_d(&st, &dt, depth + 1) {
                                return false;
                            }
                        }
                        None => {
                            if !dp.optional {
                                if let Some(si) = &s.index {
                                    if self.assignable_d(si, &dp.ty, depth + 1) {
                                        continue;
                                    }
                                }
                                return false;
                            }
                        }
                    }
                }
                if let Some(di) = &d.index {
                    for sp in s.props.values() {
                        if !self.assignable_d(&sp.ty, di, depth + 1) {
                            return false;
                        }
                    }
                    if let Some(si) = &s.index {
                        if !self.assignable_d(si, di, depth + 1) {
                            return false;
                        }
                    }
                }
                true
            }
            (Ty::Func(s), Ty::Func(d)) => {
                let s_req = s.params.iter().filter(|p| !p.optional && !p.rest).count();
                if s_req > d.params.iter().filter(|p| !p.rest).count() && !d.params.iter().any(|p| p.rest) {
                    return false;
                }
                for (i, dp) in d.params.iter().enumerate() {
                    if let Some(sp) = s.params.get(i) {
                        if sp.rest {
                            break;
                        }
                        let sty = erase_params(&sp.ty);
                        let dty = erase_params(&dp.ty);
                        if !self.assignable_d(&dty, &sty, depth + 1) && !self.assignable_d(&sty, &dty, depth + 1) {
                            return false;
                        }
                    }
                }
                if matches!(d.ret, Ty::Void) {
                    return true;
                }
                let sr = erase_params(&s.ret);
                let dr = erase_params(&d.ret);
                self.assignable_d(&sr, &dr, depth + 1)
            }
            _ => false,
        }
    }
}

/// Replace unresolved generic parameters with `any` (used when comparing generic functions).
pub fn erase_params(t: &Ty) -> Ty {
    match t {
        Ty::Param(_) => Ty::Any,
        Ty::Array(e) => Ty::Array(Rc::new(erase_params(e))),
        Ty::Tuple(v) => Ty::Tuple(Rc::new(v.iter().map(erase_params).collect())),
        Ty::Promise(e) => Ty::Promise(Rc::new(erase_params(e))),
        Ty::Union(u) => mk_union(u.iter().map(erase_params).collect()),
        Ty::Obj(o) => Ty::Obj(Rc::new(ObjTy {
            props: o.props.iter().map(|(k, p)| (k.clone(), Prop { ty: erase_params(&p.ty), optional: p.optional })).collect(),
            index: o.index.as_ref().map(erase_params),
        })),
        Ty::Func(f) => Ty::Func(Rc::new(FnTy {
            tparams: vec![],
            params: f.params.iter().map(|p| ParamTy { name: p.name.clone(), ty: erase_params(&p.ty), optional: p.optional, rest: p.rest }).collect(),
            ret: erase_params(&f.ret),
        })),
        other => other.clone(),
    }
}
