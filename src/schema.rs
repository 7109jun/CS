use crate::types::*;
use serde_json::{json, Map, Value as Json};

struct Ctx {
    defs: Map<String, Json>,
    in_progress: Vec<usize>,
    used_refs: Vec<usize>,
}

fn def_name(tcx: &TypeCtx, id: usize) -> String {
    format!("{}_{}", tcx.ifaces[id].name, id)
}

pub fn ty_schema(tcx: &TypeCtx, ty: &Ty) -> Result<Json, String> {
    let mut ctx = Ctx { defs: Map::new(), in_progress: vec![], used_refs: vec![] };
    let mut s = go(tcx, ty, &mut ctx, 0)?;
    if !ctx.defs.is_empty() {
        if let Json::Object(m) = &mut s {
            m.insert("$defs".into(), Json::Object(ctx.defs));
        } else {
            s = json!({ "allOf": [s], "$defs": Json::Object(ctx.defs) });
        }
    }
    Ok(s)
}

fn go(tcx: &TypeCtx, ty: &Ty, ctx: &mut Ctx, depth: u32) -> Result<Json, String> {
    if depth > 32 {
        return Err("type is too deeply nested to describe as JSON Schema".into());
    }
    Ok(match ty {
        Ty::Number => json!({"type": "number"}),
        Ty::Str => json!({"type": "string"}),
        Ty::Bool => json!({"type": "boolean"}),
        Ty::Null | Ty::Void => json!({"type": "null"}),
        Ty::LitStr(s) => json!({"type": "string", "const": s.to_string()}),
        Ty::LitNum(n) => json!({"type": "number", "const": n}),
        Ty::LitBool(b) => json!({"type": "boolean", "const": b}),
        Ty::Any | Ty::Unknown | Ty::Error => json!({}),
        Ty::Never => json!({"not": {}}),
        Ty::Array(e) => json!({"type": "array", "items": go(tcx, e, ctx, depth + 1)?}),
        Ty::Tuple(v) => {
            let items: Result<Vec<Json>, String> = v.iter().map(|t| go(tcx, t, ctx, depth + 1)).collect();
            json!({"type": "array", "prefixItems": items?, "items": false, "minItems": v.len(), "maxItems": v.len()})
        }
        Ty::Obj(o) => {
            let mut props = Map::new();
            let mut required = Vec::new();
            for (k, p) in o.props.iter() {
                props.insert(k.clone(), go(tcx, &p.ty, ctx, depth + 1)?);
                if !p.optional {
                    required.push(Json::String(k.clone()));
                }
            }
            let mut m = Map::new();
            m.insert("type".into(), json!("object"));
            m.insert("properties".into(), Json::Object(props));
            if !required.is_empty() {
                m.insert("required".into(), Json::Array(required));
            }
            if let Some(i) = &o.index {
                m.insert("additionalProperties".into(), go(tcx, i, ctx, depth + 1)?);
            }
            Json::Object(m)
        }
        Ty::Union(u) => {
            // all string literals -> enum
            if u.iter().all(|m| matches!(m, Ty::LitStr(_))) {
                let vals: Vec<Json> = u.iter().map(|m| if let Ty::LitStr(s) = m { json!(s.to_string()) } else { Json::Null }).collect();
                return Ok(json!({"type": "string", "enum": vals}));
            }
            let parts: Result<Vec<Json>, String> = u.iter().map(|t| go(tcx, t, ctx, depth + 1)).collect();
            json!({"anyOf": parts?})
        }
        Ty::Ref(id, args) => {
            if ctx.in_progress.contains(id) {
                if !ctx.used_refs.contains(id) {
                    ctx.used_refs.push(*id);
                }
                return Ok(json!({"$ref": format!("#/$defs/{}", def_name(tcx, *id))}));
            }
            ctx.in_progress.push(*id);
            let body = go(tcx, &Ty::Obj(tcx.expand(*id, args)), ctx, depth + 1)?;
            ctx.in_progress.pop();
            if ctx.used_refs.contains(id) {
                ctx.defs.insert(def_name(tcx, *id), body);
                json!({"$ref": format!("#/$defs/{}", def_name(tcx, *id))})
            } else {
                body
            }
        }
        Ty::Param(p) => match tcx.constraints.get(p) {
            Some(c) => go(tcx, c, ctx, depth + 1)?,
            None => json!({}),
        },
        Ty::Func(_) => return Err("function types cannot be described by JSON Schema".into()),
        Ty::Promise(_) => return Err("Promise types cannot be described by JSON Schema".into()),
    })
}

// ---------------- runtime validation ----------------

fn type_of(v: &Json) -> &'static str {
    match v {
        Json::Null => "null",
        Json::Bool(_) => "boolean",
        Json::Number(_) => "number",
        Json::String(_) => "string",
        Json::Array(_) => "array",
        Json::Object(_) => "object",
    }
}

fn describe(v: &Json) -> String {
    let s = v.to_string();
    if s.chars().count() > 40 {
        format!("{}…", s.chars().take(40).collect::<String>())
    } else {
        s
    }
}

pub fn validate(schema: &Json, value: &Json) -> Result<(), String> {
    validate_at(schema, value, schema, "", 0)
}

fn validate_at(schema: &Json, value: &Json, root: &Json, path: &str, depth: u32) -> Result<(), String> {
    if depth > 64 {
        return Err(format!("{}: value is nested too deeply", disp(path)));
    }
    let Json::Object(s) = schema else {
        return match schema {
            Json::Bool(false) => Err(format!("{}: value not allowed", disp(path))),
            _ => Ok(()),
        };
    };
    if let Some(Json::String(r)) = s.get("$ref") {
        let name = r.trim_start_matches("#/$defs/");
        let target = root.get("$defs").and_then(|d| d.get(name)).ok_or_else(|| format!("unresolved schema reference {}", r))?;
        return validate_at(target, value, root, path, depth + 1);
    }
    if let Some(c) = s.get("const") {
        let same = match (c, value) {
            (Json::Number(a), Json::Number(b)) => a.as_f64() == b.as_f64(),
            _ => c == value,
        };
        if !same {
            return Err(format!("{}: expected {}, got {}", disp(path), describe(c), describe(value)));
        }
    }
    if let Some(Json::Array(e)) = s.get("enum") {
        if !e.contains(value) {
            let opts: Vec<String> = e.iter().map(|x| x.to_string()).collect();
            return Err(format!("{}: expected one of {}, got {}", disp(path), opts.join(", "), describe(value)));
        }
    }
    if let Some(t) = s.get("type") {
        let ok = match t {
            Json::String(t) => type_matches(t, value),
            Json::Array(ts) => ts.iter().any(|t| t.as_str().map_or(false, |t| type_matches(t, value))),
            _ => true,
        };
        if !ok {
            return Err(format!("{}: expected {}, got {}", disp(path), t.as_str().unwrap_or("value"), type_of(value)));
        }
    }
    if let Some(Json::Array(opts)) = s.get("anyOf") {
        let mut errs = Vec::new();
        let mut ok = false;
        for o in opts {
            match validate_at(o, value, root, path, depth + 1) {
                Ok(()) => {
                    ok = true;
                    break;
                }
                Err(e) => errs.push(e),
            }
        }
        if !ok {
            let first = errs.into_iter().next().unwrap_or_default();
            return Err(format!("{}: value matches none of the allowed types (e.g. {})", disp(path), first));
        }
    }
    if let Json::Object(obj) = value {
        if let Some(Json::Array(req)) = s.get("required") {
            for r in req {
                if let Some(k) = r.as_str() {
                    if !obj.contains_key(k) {
                        return Err(format!("{}: missing required property '{}'", disp(path), k));
                    }
                }
            }
        }
        let props = s.get("properties").and_then(|p| p.as_object());
        for (k, v) in obj {
            let sub_path = format!("{}/{}", path, k);
            if let Some(ps) = props.and_then(|p| p.get(k)) {
                validate_at(ps, v, root, &sub_path, depth + 1)?;
            } else if let Some(ap) = s.get("additionalProperties") {
                match ap {
                    Json::Bool(false) => return Err(format!("{}: unexpected property '{}'", disp(path), k)),
                    Json::Bool(true) => {}
                    other => validate_at(other, v, root, &sub_path, depth + 1)?,
                }
            }
        }
    }
    if let Json::Array(arr) = value {
        if let Some(Json::Number(n)) = s.get("minItems") {
            if (arr.len() as u64) < n.as_u64().unwrap_or(0) {
                return Err(format!("{}: expected at least {} items", disp(path), n));
            }
        }
        if let Some(Json::Number(n)) = s.get("maxItems") {
            if (arr.len() as u64) > n.as_u64().unwrap_or(u64::MAX) {
                return Err(format!("{}: expected at most {} items", disp(path), n));
            }
        }
        let prefix = s.get("prefixItems").and_then(|p| p.as_array());
        for (i, v) in arr.iter().enumerate() {
            let sub_path = format!("{}/{}", path, i);
            if let Some(ps) = prefix.and_then(|p| p.get(i)) {
                validate_at(ps, v, root, &sub_path, depth + 1)?;
            } else if let Some(items) = s.get("items") {
                validate_at(items, v, root, &sub_path, depth + 1)?;
            }
        }
    }
    Ok(())
}

fn disp(path: &str) -> String {
    if path.is_empty() {
        "value".into()
    } else {
        path.to_string()
    }
}

fn type_matches(t: &str, v: &Json) -> bool {
    match t {
        "integer" => v.as_f64().map_or(false, |n| n.fract() == 0.0),
        "number" => v.is_number(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn validates_types_required_and_enums() {
        let s = json!({"type":"object","properties":{"n":{"type":"number"},"k":{"enum":["a","b"]}},"required":["n"],"additionalProperties":false});
        assert!(validate(&s, &json!({"n": 1, "k": "a"})).is_ok());
        assert!(validate(&s, &json!({})).is_err());
        assert!(validate(&s, &json!({"n": "x"})).is_err());
        assert!(validate(&s, &json!({"n": 1, "k": "z"})).is_err());
        assert!(validate(&s, &json!({"n": 1, "extra": 1})).is_err());
    }
}
